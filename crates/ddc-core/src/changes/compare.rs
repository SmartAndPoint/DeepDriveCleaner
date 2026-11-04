//! The comparison itself: pure, over two decoded reports and the journal's
//! lines, so every rule is tested on hand-built reports.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::journal::JournalLine;
use super::not_read::not_read;
use super::{
    CHANGES_SCHEMA_VERSION, ChangesReport, Cleaned, Disk, Group, HarnessDelta, Home, HomeReason, Item, ItemKind,
    Loaded, NotRead, NotReadKind, Reason, Rest, Side, Status,
};
use crate::model::{Finding, OriginHarness, ScanReport};
use crate::rules::listing::group_of;
use crate::rules::worktree::{ORPHAN_RULE, WORKTREE_RULE};
use crate::scan::WalkOptions;

/// The label of a rule in no Review group.
const OTHER: &str = "Other";
/// The rule of the home's Trash, whose growth may be the person's trashed items.
const TRASH_RULE: &str = "trash";
const HOMEBREW_RULE: &str = "cache.homebrew";

/// [`compare`], after the reasons the reading found and those the reports
/// tell (`predates-fields`, `cancelled`, `no-shared-root`); the first in
/// [`Reason`]'s order wins.
pub fn compare_loaded(loaded: &Loaded, journal: &[JournalLine], cloud: &[PathBuf]) -> ChangesReport {
    let mut reasons = loaded.reasons.clone();
    let sides = [&loaded.baseline, &loaded.current];
    if sides.iter().any(|r| r.as_ref().is_some_and(|r| r.written_at.is_none() || r.rules_in_force.is_none())) {
        reasons.push(Reason::PredatesFields);
    }
    if sides.iter().any(|r| r.as_ref().is_some_and(|r| r.cancelled)) {
        reasons.push(Reason::Cancelled);
    }
    if let (Some(b), Some(c)) = (&loaded.baseline, &loaded.current)
        && compared_roots(b, c).is_empty()
    {
        reasons.push(Reason::NoSharedRoot);
    }
    match (reasons.into_iter().min(), &loaded.baseline, &loaded.current) {
        (None, Some(b), Some(c)) => compare(b, c, journal, cloud),
        (reason, b, c) => ChangesReport::not_compared(
            reason.unwrap_or(Reason::NoReport),
            b.as_ref().map(Side::of),
            c.as_ref().map(Side::of),
        ),
    }
}

/// Compares `baseline` with `current`, two reports that can be compared:
/// both carry S1's fields, neither is cancelled, and they share a root.
pub fn compare(
    baseline: &ScanReport,
    current: &ScanReport,
    journal: &[JournalLine],
    cloud: &[PathBuf],
) -> ChangesReport {
    let roots = compared_roots(baseline, current);
    let in_scope = |f: &&Finding| roots.iter().any(|r| f.path.starts_with(r));
    let before: Vec<&Finding> = baseline.findings.iter().filter(in_scope).collect();
    let now: Vec<&Finding> = current.findings.iter().filter(in_scope).collect();
    let not_read_before = not_read(baseline, cloud);
    let not_read_now = not_read(current, cloud);
    let rules: HashSet<&str> = shared_rules(baseline, current);

    let credits = credits(baseline, &before, current, journal);
    let (wt_before, wt_now) = (worktrees(baseline), worktrees(current));
    let now_by_key: HashMap<(&Path, &str), &Finding> =
        now.iter().map(|f| ((f.path.as_path(), f.rule.as_str()), *f)).collect();
    let before_keys: HashSet<(&Path, &str)> = before.iter().map(|f| (f.path.as_path(), f.rule.as_str())).collect();
    // A finding one side could not have seen: under a folder the other did
    // not read, or of a rule the other did not have.
    let compared = |f: &Finding, other: &[NotRead]| rules.contains(f.rule.as_str()) && !under(&f.path, other);

    let mut rows: Vec<Row> = Vec::new();
    let mut gone: Vec<&Finding> = Vec::new();
    for b in &before {
        match now_by_key.get(&(b.path.as_path(), b.rule.as_str())) {
            Some(c) if !compared(b, &not_read_now) || !compared(c, &not_read_before) => {
                rows.push(not_compared(Some(b), Some(c), &wt_now));
            }
            Some(c) => rows.extend(matched(b, c, &wt_now, credits.get(&key(b)))),
            None if !compared(b, &not_read_now) => rows.push(not_compared(Some(b), None, &wt_before)),
            None => gone.push(b),
        }
    }
    let mut new: Vec<&Finding> = Vec::new();
    for c in &now {
        if before_keys.contains(&(c.path.as_path(), c.rule.as_str())) {
            continue;
        }
        if compared(c, &not_read_before) {
            new.push(c);
        } else {
            rows.push(not_compared(None, Some(c), &wt_now));
        }
    }
    // A gone and a new finding of one identity and rule: moved.
    // A journaled clean removed it where it was: it did not move.
    let mut by_identity: HashMap<(u64, u64, &str), &Finding> =
        gone.iter().filter(|b| !credits.contains_key(&key(b))).map(|b| ((b.dev, b.ino, b.rule.as_str()), *b)).collect();
    let mut moved: HashSet<(&Path, &str)> = HashSet::new();
    for c in new {
        let item = match by_identity.remove(&(c.dev, c.ino, c.rule.as_str())) {
            Some(b) => {
                moved.insert((b.path.as_path(), b.rule.as_str()));
                Item { kind: ItemKind::Moved, moved_from: Some(b.path.clone()), ..row(Some(b), Some(c), &wt_now) }
            }
            None => Item { kind: ItemKind::New, ..row(None, Some(c), &wt_now) },
        };
        rows.push(Row::from(item));
    }
    for b in gone.into_iter().filter(|b| !moved.contains(&(b.path.as_path(), b.rule.as_str()))) {
        let mut item = Item { kind: ItemKind::Gone, ..row(Some(b), None, &wt_before) };
        let mut trashed_part = None;
        // A named finding that is gone was cleaned whole.
        if let Some(credit) = credits.get(&key(b)) {
            item.kind = ItemKind::Cleaned;
            item.cleaned = b.bytes;
            trashed_part = Some(b.bytes.min(credit.trashed));
        }
        rows.push(Row { item, trashed_part });
    }

    let mut cleaned_since = Cleaned::default();
    for r in &rows {
        if let Some(trashed) = r.trashed_part {
            cleaned_since.trashed += trashed;
            cleaned_since.deleted += r.item.cleaned - trashed;
        }
    }
    // The Trash's growth up to what the person trashed is their own.
    let mut pool = cleaned_since.trashed;
    for r in rows.iter_mut().filter(|r| r.item.rule == TRASH_RULE && r.item.kind != ItemKind::NotCompared) {
        if r.item.delta > 0 && pool > 0 {
            let taken = pool.min(r.item.delta.unsigned_abs());
            pool -= taken;
            r.item.trashed_in = Some(taken);
        }
    }

    let items: Vec<Item> = sorted(rows.into_iter().map(|r| r.item).collect());
    let groups = groups(&items);
    let ignored_delta: i64 = items.iter().filter(|i| i.ignored && counts(i)).map(net).sum();
    let compared_delta: i64 = items.iter().filter(|i| counts(i)).map(|i| i.delta).sum();
    let home = home(baseline, current, &not_read_before, &not_read_now, ignored_delta, compared_delta);
    let disk = disk(baseline, current, &roots);
    let rest =
        Rest { delta: disk.as_ref().map(|d| d.delta - home.delta.unwrap_or(compared_delta)), not_read: not_read_now };
    ChangesReport {
        schema_version: CHANGES_SCHEMA_VERSION,
        status: Status::Compared,
        reason: None,
        baseline: Some(Side::of(baseline)),
        current: Some(Side::of(current)),
        compared_roots: roots,
        disk,
        home: Some(home),
        rest: Some(rest),
        cleaned_since: Some(cleaned_since),
        groups,
        items,
    }
}

/// A row, with the trashed part of its cleaned bytes while the sums are made.
struct Row {
    item: Item,
    trashed_part: Option<u64>,
}

impl From<Item> for Row {
    fn from(item: Item) -> Self {
        Self { item, trashed_part: None }
    }
}

/// What the journal credits a baseline finding: the bytes of the lines that
/// name it, and of those the trashed ones.
#[derive(Debug, Clone, Copy, Default)]
struct Credit {
    bytes: u64,
    trashed: u64,
}

type Key = (PathBuf, String);
/// The harness of each worktree finding of a report, by its path.
type Worktrees<'a> = HashMap<&'a Path, OriginHarness>;

fn key(f: &Finding) -> Key {
    (f.path.clone(), f.rule.clone())
}

/// The journal's lines between the two saves, credited to the baseline
/// findings they name.
fn credits(
    baseline: &ScanReport,
    before: &[&Finding],
    current: &ScanReport,
    journal: &[JournalLine],
) -> HashMap<Key, Credit> {
    let from = at(baseline.written_at.as_deref());
    let to = at(current.written_at.as_deref());
    let in_window = |l: &JournalLine| match (l.at(), from, to) {
        (Some(t), Some(from), Some(to)) => t >= from && t <= to,
        _ => false,
    };
    let folders: HashMap<&Path, &Finding> =
        before.iter().filter(|f| f.contents.is_some()).map(|f| (f.path.as_path(), *f)).collect();
    let by_identity: HashMap<(&Path, u64, u64), &Finding> =
        before.iter().map(|f| ((f.path.as_path(), f.dev, f.ino), *f)).collect();
    let mut out: HashMap<Key, Credit> = HashMap::new();
    for line in journal.iter().filter(|l| in_window(l)) {
        if line.brew_cleanup_ran() {
            if let Some(f) = before.iter().find(|f| f.rule == HOMEBREW_RULE) {
                let freed = match (line.bytes_before, line.bytes_after) {
                    (Some(b), Some(a)) => b.saturating_sub(a),
                    _ => u64::MAX,
                };
                let c = out.entry(key(f)).or_default();
                c.bytes = c.bytes.saturating_add(freed);
            }
            continue;
        }
        if !line.removed() {
            continue;
        }
        let Some(path) = line.path.as_deref() else { continue };
        let named = match (line.dev, line.ino) {
            (Some(dev), Some(ino)) => by_identity.get(&(path, dev, ino)).copied(),
            _ => None,
        };
        // A picked entry of a folder, or a session's companion by its entry.
        let named = named
            .or_else(|| path.parent().and_then(|p| folders.get(p).copied()))
            .or_else(|| line.entry.as_deref().and_then(Path::parent).and_then(|p| folders.get(p).copied()));
        let Some(f) = named else { continue };
        let bytes = line.bytes.unwrap_or(0);
        let c = out.entry(key(f)).or_default();
        c.bytes = c.bytes.saturating_add(bytes);
        if line.trashed() {
            c.trashed = c.trashed.saturating_add(bytes);
        }
    }
    out
}

fn at(rfc3339: Option<&str>) -> Option<SystemTime> {
    humantime::parse_rfc3339_weak(rfc3339?).ok()
}

/// A finding in both reports: grew, shrank (by the person's cleans, wholly
/// or in part) or unchanged, which is no row.
fn matched(b: &Finding, c: &Finding, worktrees: &Worktrees, credit: Option<&Credit>) -> Option<Row> {
    let mut item = row(Some(b), Some(c), worktrees);
    if item.delta == 0 {
        return None;
    }
    item.kind = if item.delta > 0 { ItemKind::Grew } else { ItemKind::Shrank };
    let mut r = Row::from(item);
    if r.item.delta < 0
        && let Some(credit) = credit
    {
        let shrink = r.item.delta.unsigned_abs();
        r.item.cleaned = shrink.min(credit.bytes);
        if r.item.cleaned == shrink {
            r.item.kind = ItemKind::Cleaned;
        }
        r.trashed_part = Some(r.item.cleaned.min(credit.trashed));
    }
    Some(r)
}

fn not_compared(b: Option<&Finding>, c: Option<&Finding>, worktrees: &Worktrees) -> Row {
    Row::from(Item { kind: ItemKind::NotCompared, ..row(b, c, worktrees) })
}

/// A row of `b` and `c`, its kind to be set; its harness and `ignored` are
/// the current finding's, or the baseline's for a finding that is gone.
/// `worktrees` are those of that finding's report.
fn row(b: Option<&Finding>, c: Option<&Finding>, worktrees: &Worktrees) -> Item {
    let f = c.or(b).expect("a finding on one side");
    let before = b.map(|f| f.bytes);
    let now = c.map(|f| f.bytes);
    Item {
        path: f.path.clone(),
        rule: f.rule.clone(),
        kind: ItemKind::NotCompared,
        before,
        now,
        delta: now.unwrap_or(0) as i64 - before.unwrap_or(0) as i64,
        cleaned: 0,
        trashed_in: None,
        moved_from: None,
        harness: harness(f, worktrees),
        ignored: f.ignored,
        approximate: b.is_some_and(|f| f.shared_bytes > 0) || c.is_some_and(|f| f.shared_bytes > 0),
    }
}

/// The coding agent of a finding: its own `origin`, else that of the
/// worktree finding at its project's root, in the same report.
fn harness(f: &Finding, worktrees: &Worktrees) -> Option<OriginHarness> {
    if let Some(origin) = &f.origin {
        return Some(origin.harness);
    }
    worktrees.get(f.project.as_ref()?.root.as_path()).copied()
}

fn worktrees(report: &ScanReport) -> Worktrees<'_> {
    report
        .findings
        .iter()
        .filter(|w| w.rule == WORKTREE_RULE || w.rule == ORPHAN_RULE)
        .filter_map(|w| Some((w.path.as_path(), w.origin.as_ref()?.harness)))
        .collect()
}

/// A row that counts in the groups or `ignored_delta`.
fn counts(i: &Item) -> bool {
    i.kind != ItemKind::NotCompared
}

/// A row's change with the person's cleans and the Trash's intake left out.
fn net(i: &Item) -> i64 {
    i.delta + i.cleaned as i64 - i.trashed_in.unwrap_or(0) as i64
}

fn groups(items: &[Item]) -> Vec<Group> {
    let mut by: BTreeMap<&str, Group> = BTreeMap::new();
    for i in items.iter().filter(|i| counts(i) && !i.ignored) {
        let label = group_of(&i.rule).unwrap_or(OTHER);
        let g = by.entry(label).or_insert_with(|| Group {
            group: label.to_owned(),
            delta: 0,
            new: 0,
            grew: 0,
            shrank: 0,
            gone: 0,
            cleaned: 0,
            moved: 0,
            by_harness: Vec::new(),
        });
        g.delta += net(i);
        match i.kind {
            ItemKind::New => g.new += 1,
            ItemKind::Grew => g.grew += 1,
            ItemKind::Shrank => g.shrank += 1,
            ItemKind::Gone => g.gone += 1,
            ItemKind::Cleaned => g.cleaned += 1,
            ItemKind::Moved => g.moved += 1,
            ItemKind::NotCompared => {}
        }
        if let Some(h) = i.harness {
            match g.by_harness.iter_mut().find(|d| d.harness == h) {
                Some(d) => {
                    d.delta += net(i);
                    d.count += 1;
                }
                None => g.by_harness.push(HarnessDelta { harness: h, delta: net(i), count: 1 }),
            }
        }
    }
    let mut out: Vec<Group> = by.into_values().collect();
    for g in &mut out {
        g.by_harness.sort_by_key(|d| std::cmp::Reverse(d.delta));
    }
    out.sort_by(|a, b| b.delta.cmp(&a.delta).then_with(|| a.group.cmp(&b.group)));
    out
}

/// Biggest change first, then by path.
fn sorted(mut items: Vec<Item>) -> Vec<Item> {
    items.sort_by(|a, b| {
        let size = |i: &Item| i.delta.unsigned_abs().max(i.cleaned);
        size(b).cmp(&size(a)).then_with(|| a.path.cmp(&b.path))
    });
    items
}

/// The roots both reports read: the reports' own when they are equal, else
/// each root of one at or inside a root of the other. Empty when none.
fn compared_roots(baseline: &ScanReport, current: &ScanReport) -> Vec<PathBuf> {
    let (b, c) = (&baseline.scan_roots, &current.scan_roots);
    if same(b, c) {
        return c.clone();
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for x in b {
        for y in c {
            let inner = if y.starts_with(x) {
                y
            } else if x.starts_with(y) {
                x
            } else {
                continue;
            };
            if !out.contains(inner) {
                out.push(inner.clone());
            }
        }
    }
    out.sort();
    out
}

fn same(a: &[PathBuf], b: &[PathBuf]) -> bool {
    let (mut a, mut b) = (a.to_vec(), b.to_vec());
    a.sort();
    b.sort();
    a == b
}

/// The rules both scans had. A rule new in this version, or gone from it,
/// says nothing about growth.
fn shared_rules<'a>(baseline: &'a ScanReport, current: &ScanReport) -> HashSet<&'a str> {
    let now: HashSet<&str> = current.rules_in_force.iter().flatten().map(String::as_str).collect();
    baseline.rules_in_force.iter().flatten().map(String::as_str).filter(|r| now.contains(r)).collect()
}

fn under(path: &Path, not_read: &[NotRead]) -> bool {
    not_read.iter().any(|n| path.starts_with(&n.path))
}

fn home(
    baseline: &ScanReport,
    current: &ScanReport,
    not_read_before: &[NotRead],
    not_read_now: &[NotRead],
    ignored_delta: i64,
    compared_delta: i64,
) -> Home {
    let mut reasons = Vec::new();
    if baseline.home != current.home {
        reasons.push(HomeReason::DifferentHome);
    }
    if !same(&baseline.scan_roots, &current.scan_roots) {
        reasons.push(HomeReason::DifferentRoots);
    }
    if baseline.full_disk_access != current.full_disk_access {
        reasons.push(HomeReason::DifferentAccess);
    }
    if cut(baseline) || cut(current) {
        reasons.push(HomeReason::CutList);
    }
    let read = |n: &[NotRead]| -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> =
            n.iter().filter(|n| n.kind != NotReadKind::Changed).map(|n| n.path.clone()).collect();
        paths.sort();
        paths.dedup();
        paths
    };
    if read(not_read_before) != read(not_read_now) {
        reasons.push(HomeReason::DifferentNotRead);
    }
    let compared = reasons.is_empty();
    let delta = compared.then(|| current.totals.scanned_bytes as i64 - baseline.totals.scanned_bytes as i64);
    Home { compared, reasons, delta, ignored_delta, elsewhere: delta.map(|d| d - compared_delta) }
}

/// A report whose `skipped` or `inaccessible` list was cut short.
fn cut(report: &ScanReport) -> bool {
    report.skipped_total > report.skipped.len() as u64
        || report.inaccessible.len() >= WalkOptions::default().max_recorded
}

/// The home folder's volume in both reports: the one whose roots hold the
/// home, else the one of the compared roots.
fn disk(baseline: &ScanReport, current: &ScanReport, roots: &[PathBuf]) -> Option<Disk> {
    let home = &current.home;
    let now = current
        .volumes
        .iter()
        .find(|v| v.roots.iter().any(|r| home.starts_with(r)))
        .or_else(|| current.volumes.iter().find(|v| v.roots.iter().any(|r| roots.contains(r))))?;
    let before = baseline.volumes.iter().find(|v| v.mount_point == now.mount_point)?;
    let in_use = |v: &crate::model::Volume| v.total_bytes.saturating_sub(v.free_bytes);
    Some(Disk {
        mount_point: now.mount_point.clone(),
        before_in_use: in_use(before),
        now_in_use: in_use(now),
        delta: in_use(now) as i64 - in_use(before) as i64,
        before_at: before.read_at.clone().unwrap_or_else(|| baseline.generated_at.clone()),
        now_at: now.read_at.clone().unwrap_or_else(|| current.generated_at.clone()),
    })
}
