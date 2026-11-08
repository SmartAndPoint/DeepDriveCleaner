//! Human-readable scan, disk, agents and disk-free reports.

use std::collections::{BTreeMap, HashSet};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use ddc_core::agents::{
    AgentsReport, FootprintState, RecordsState, RunningKind, SessionFootprint, SessionState, TempRefusal, TempRoot,
    UnknownReason,
};
use ddc_core::brew::{BrewPackages, NotOffered, Service, ServicesState};
use ddc_core::class::{Category, RiskClass};
use ddc_core::disk::{ActionId, DiskReport, HeldKind, HomeSource, SimulatorStatus};
use ddc_core::docker::{DockerContents, DockerResult, Group};
use ddc_core::footprint::{
    Bucket, CheckoutEntry, CheckoutKind as FootprintKind, FootprintReport, HeldReason, Lock, NotScannedReason,
};
use ddc_core::free::{ActionReport, ActionResult, FreeReport, Refusal, ReportEdit};
use ddc_core::model::{Finding, NotSelected, OriginHarness, ScanReport};
use ddc_core::rules::Note;
use ddc_core::rules::worktree::{self, ORPHAN_RULE, WORKTREE_RULE};

use crate::size::{count, human};

/// Findings shown per group unless `--all`.
const PER_GROUP: usize = 8;

/// Sessions `ddc agent status` lists per harness; `--json` has them all.
const TOP_SESSIONS: usize = 10;

pub struct Style {
    pub color: bool,
    pub all: bool,
}

impl Style {
    fn paint(&self, code: &str, text: &str) -> String {
        if self.color { format!("\x1b[{code}m{text}\x1b[0m") } else { text.to_owned() }
    }
    pub fn bold(&self, text: &str) -> String {
        self.paint("1", text)
    }
    pub fn dim(&self, text: &str) -> String {
        self.paint("2", text)
    }
    fn class(&self, class: RiskClass, text: &str) -> String {
        let code = match class {
            RiskClass::Safe => "1;32",
            RiskClass::Rebuildable => "1;36",
            RiskClass::Review => "1;33",
            RiskClass::UserData => "1;35",
        };
        self.paint(code, text)
    }
}

fn class_title(class: RiskClass) -> &'static str {
    match class {
        RiskClass::Safe => "SAFE — caches that refill themselves",
        RiskClass::Rebuildable => "REBUILDABLE — project artifacts restored by install or build",
        RiskClass::Review => "REVIEW — expensive to restore; your call, one by one",
        RiskClass::UserData => "USER DATA — never cleaned by default; files you pick go to the Trash",
    }
}

/// `/Users/me/x` → `~/x`. Control characters are escaped so a crafted file
/// name cannot move the cursor or recolor a confirmation prompt, and
/// bidirectional format characters so `cod.exe\u{202e}fdp.` cannot pass for a PDF.
pub fn tilde(path: &Path, home: &Path) -> String {
    let shown = match path.strip_prefix(home) {
        Ok(rel) if rel.as_os_str().is_empty() => "~".to_owned(),
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => path.display().to_string(),
    };
    escaped(&shown)
}

/// `ddc clean` over `roots`, each one shell word, so it can be pasted as is
/// into any POSIX `sh` and names the same paths. A newline or a tab stays
/// literal inside the word's single quotes (dash has no `$'…'`). `None` when
/// a root holds a byte that is not UTF-8, another control character or a
/// bidirectional format character: shown escaped it would name another
/// path, shown raw it could move the cursor, so no command is suggested.
pub fn clean_command(roots: &[PathBuf], home: &Path) -> Option<String> {
    let mut command = "ddc clean".to_owned();
    for root in roots {
        let (prefix, rest) = match root.strip_prefix(home) {
            Ok(rel) if rel.as_os_str().is_empty() => ("~", None),
            Ok(rel) => ("~/", Some(rel.to_str()?)),
            Err(_) => ("", Some(root.to_str()?)),
        };
        if rest.is_some_and(|w| w.chars().any(|c| (c.is_control() && c != '\n' && c != '\t') || bidi(c))) {
            return None;
        }
        command.push(' ');
        command.push_str(prefix);
        command.push_str(&rest.map(quoted).unwrap_or_default());
    }
    Some(command)
}

/// The text report's last step: the command to run, or, when no command can
/// be pasted, what to pass by hand.
pub fn clean_hint(roots: &[PathBuf], home: &Path) -> String {
    match clean_command(roots, home) {
        Some(command) => format!("run `{command}` to review and confirm"),
        None => "review and confirm with ddc clean and the scanned folder (its name cannot be pasted)".to_owned(),
    }
}

/// One word for a POSIX shell: as is when it is plain, else in single quotes.
fn quoted(word: &str) -> String {
    let plain = !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "_./=:@%+-,".contains(c));
    if plain { word.to_owned() } else { format!("'{}'", word.replace('\'', r"'\''")) }
}

/// `text` with control and bidirectional format characters escaped, as
/// [`tilde`] shows a path: for text another program wrote.
pub fn escaped(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() || bidi(c) { c.escape_default().to_string() } else { c.to_string() })
        .collect()
}

/// Bidirectional embeddings, overrides and isolates, and the direction marks.
fn bidi(c: char) -> bool {
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{061c}')
}

/// A volume's name inside a sentence: only a leading "The " is lower-cased.
pub fn mid_sentence(name: &str) -> String {
    match name.strip_prefix("The ") {
        Some(rest) => format!("the {rest}"),
        None => name.to_owned(),
    }
}

/// "just now", "5 min ago", "2 h ago", "3 d ago": `at` seen from `now`, with
/// the thresholds of the app's `Wording.age`. `None` when either time cannot
/// be read.
pub fn age(at: &str, now: &str) -> Option<String> {
    let secs = |t: &str| -> Option<i128> {
        let t = humantime::parse_rfc3339_weak(t).ok()?;
        Some(match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => i128::from(d.as_secs()),
            Err(e) => -i128::from(e.duration().as_secs()),
        })
    };
    let minutes = (secs(now)? - secs(at)?) / 60;
    Some(match minutes {
        ..1 => "just now".to_owned(),
        1..60 => format!("{minutes} min ago"),
        60..2880 => format!("{} h ago", minutes / 60),
        _ => format!("{} d ago", minutes / 1440),
    })
}

/// One line per lock holder, as `ddc lock status` and a busy command print
/// it: "pid 4101: clean --items, started 3 min ago from a terminal, on
/// item 4 of 8".
pub fn holder(h: &ddc_core::holders::Holder, now: &str) -> String {
    use ddc_core::holders::{HolderCommand, HolderPhase, Origin};
    let what = match h.command {
        HolderCommand::Scan => "scan",
        HolderCommand::BackgroundScan => "background scan",
        HolderCommand::Clean => "clean",
        HolderCommand::CleanDryRun => "clean --dry-run",
        HolderCommand::CleanItems => "clean --items",
        HolderCommand::CleanItemsDryRun => "clean --items --dry-run",
        HolderCommand::DiskFree => "disk free",
        HolderCommand::DiskFreeDryRun => "disk free --dry-run",
        HolderCommand::KeepRemove => "keep remove",
    };
    let from = match h.origin {
        Origin::App => " from the app",
        Origin::Terminal => " from a terminal",
        Origin::Other => "",
    };
    let mut line =
        format!("pid {}: {what}, started {}{from}", h.pid, age(&h.started, now).unwrap_or_else(|| h.started.clone()));
    match (h.phase, h.progress) {
        (Some(HolderPhase::Cleaning), Some(p)) if p.done < p.total => {
            line.push_str(&format!(", on item {} of {}", p.done + 1, p.total));
        }
        (Some(HolderPhase::Cleaning), Some(_)) => line.push_str(", finishing"),
        (Some(HolderPhase::Cleaning), None) => line.push_str(", starting its first item"),
        (Some(HolderPhase::Preparing), _) => line.push_str(", re-checking its items"),
        (None, _) => {}
    }
    line
}

/// Colors only on a terminal and without `NO_COLOR`.
pub fn use_color(is_terminal: bool, no_color: bool) -> bool {
    is_terminal && !no_color
}

fn status(f: &Finding, home: &Path) -> String {
    let mut parts = Vec::new();
    if f.rule == WORKTREE_RULE
        && let Some(project) = &f.project
    {
        parts.push(format!("worktree of {}", tilde(&project.root, home)));
    }
    if let Some(reason) = worktree_reason(f) {
        parts.push(reason);
    }
    if f.ignored {
        parts.push("ignored".to_owned());
    }
    if let Some(p) = f.private_bytes.filter(|p| *p + f.bytes / 10 < f.bytes) {
        parts.push(format!("frees ≥ {}", human(p)));
    }
    match f.not_selected {
        Some(NotSelected::ActiveProject { idle_days }) => parts.push(format!("active ({idle_days}d)")),
        Some(NotSelected::UnknownActivity) => parts.push("activity unknown".to_owned()),
        Some(NotSelected::ForeignOwner) => parts.push("another user's".to_owned()),
        Some(NotSelected::Immutable) => parts.push("immutable".to_owned()),
        _ => {}
    }
    if let Some(days) = f.project.as_ref().and_then(|p| p.idle_days).filter(|_| f.selected) {
        parts.push(format!("idle {days}d"));
    }
    for note in &f.notes {
        parts.push(
            match note {
                Note::NoLockfile => "no lock file",
                Note::AppOwned => "owned by an app",
                Note::KeptByMarker => "kept (.ddc-keep)",
                Note::Excluded => "excluded",
            }
            .to_owned(),
        );
    }
    if f.incomplete {
        parts.push("incomplete".to_owned());
    }
    parts.join(" · ")
}

/// Why a worktree is not selected: it lost its repository (both causes, as
/// its hint names them), was moved or copied, is the person's own, or the
/// condition it failed, from its hint.
fn worktree_reason(f: &Finding) -> Option<String> {
    if f.selected {
        return None;
    }
    let reason = if f.rule == ORPHAN_RULE {
        f.hint.as_str()
    } else if f.rule != WORKTREE_RULE {
        return None;
    } else if f.worktree.as_ref().is_some_and(|w| !w.linked) {
        "moved or copied: git worktree repair relinks it"
    } else if f.origin.is_some_and(|o| o.harness == OriginHarness::Git) {
        "yours: list it to remove it"
    } else {
        f.project.as_ref().and_then(|p| worktree::kept_reason(&f.hint, &p.root))?
    };
    Some(escaped(reason))
}

/// The findings inside each worktree the report lists, keyed by the
/// innermost one: shown indented under it, not in their own groups (story
/// "Worktree findings and provenance").
fn inside_worktrees(findings: &[Finding]) -> BTreeMap<&Path, Vec<&Finding>> {
    let listed = |f: &&Finding| !f.ignored;
    let worktrees: Vec<&Path> =
        findings.iter().filter(listed).filter(|f| f.rule == WORKTREE_RULE).map(|f| f.path.as_path()).collect();
    let mut inside: BTreeMap<&Path, Vec<&Finding>> = BTreeMap::new();
    for f in findings.iter().filter(listed) {
        let innermost = worktrees
            .iter()
            .filter(|wt| f.path != **wt && f.path.starts_with(wt))
            .max_by_key(|wt| wt.components().count());
        if let Some(wt) = innermost {
            inside.entry(*wt).or_default().push(f);
        }
    }
    inside
}

/// A finding's row, then what lies inside it when it is a worktree, each a
/// level deeper, its path relative to the worktree and its class named.
fn finding_rows(
    w: &mut impl Write,
    f: &Finding,
    inside: &BTreeMap<&Path, Vec<&Finding>>,
    depth: usize,
    home: &Path,
    style: &Style,
) -> io::Result<()> {
    let mark = if f.selected { "✓" } else { "·" };
    let indent = "  ".repeat(depth);
    if depth == 0 {
        writeln!(
            w,
            "    {mark} {:>10}  {}  {}  {}",
            human(f.bytes),
            tilde(&f.path, home),
            style.dim(&f.rule),
            style.dim(&status(f, home))
        )?;
    } else {
        let parent = f.path.ancestors().skip(1).find(|a| inside.contains_key(a)).unwrap_or(home);
        let notes = [f.class.as_str().to_owned(), status(f, home)].into_iter().filter(|s| !s.is_empty());
        writeln!(
            w,
            "    {indent}{mark} {:>10}  {}  {}  {}",
            human(f.bytes),
            tilde(f.path.strip_prefix(parent).unwrap_or(&f.path), home),
            style.dim(&f.rule),
            style.dim(&notes.collect::<Vec<_>>().join(" · "))
        )?;
    }
    if let Some(items) = inside.get(f.path.as_path()) {
        rows(w, items, inside, depth + 1, home, style)?;
    }
    Ok(())
}

/// `items`' rows, the largest [`PER_GROUP`] unless `--all`, then one line
/// for the rest that counts what lies inside them too, so no finding is
/// left out of both.
fn rows(
    w: &mut impl Write,
    items: &[&Finding],
    inside: &BTreeMap<&Path, Vec<&Finding>>,
    depth: usize,
    home: &Path,
    style: &Style,
) -> io::Result<()> {
    let shown = if style.all { items.len() } else { PER_GROUP.min(items.len()) };
    for f in &items[..shown] {
        finding_rows(w, f, inside, depth, home, style)?;
    }
    if shown < items.len() {
        let rest = &items[shown..];
        let bytes: u64 = rest.iter().map(|f| f.bytes).sum();
        let line = format!("… {} more, {}{} (--all shows them)", rest.len(), human(bytes), inside_them(rest, inside));
        writeln!(w, "    {}{}", "  ".repeat(depth), style.dim(&line))?;
    }
    Ok(())
}

/// How many findings lie inside `f`, at any depth, and their bytes.
fn nested(f: &Finding, inside: &BTreeMap<&Path, Vec<&Finding>>) -> (usize, u64) {
    inside.get(f.path.as_path()).map_or((0, 0), |items| {
        items.iter().fold((0, 0), |(n, b), item| {
            let (inner_n, inner_b) = nested(item, inside);
            (n + 1 + inner_n, b + item.bytes + inner_b)
        })
    })
}

/// ", and N inside them, SIZE" for what lies inside `items` at any depth;
/// empty when nothing does.
fn inside_them(items: &[&Finding], inside: &BTreeMap<&Path, Vec<&Finding>>) -> String {
    let (count, within) = items.iter().map(|f| nested(f, inside)).fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
    if count > 0 { format!(", and {count} inside them, {}", human(within)) } else { String::new() }
}

/// `own_roots`: the scan was given paths other than the default ones, so the
/// `ddc clean` it suggests names them, or that would review the home instead.
pub fn report(w: &mut impl Write, r: &ScanReport, style: &Style, own_roots: bool) -> io::Result<()> {
    let home = r.home.as_path();
    let roots: Vec<String> = r.scan_roots.iter().map(|p| tilde(p, home)).collect();
    writeln!(
        w,
        "{} · scanned {} in {:.1}s · {} files · {}",
        style.bold("DeepDriveCleaner"),
        roots.join(", "),
        r.duration_ms as f64 / 1000.0,
        count(r.totals.files),
        human(r.totals.scanned_bytes)
    )?;
    writeln!(w)?;
    for t in &r.totals.classes {
        let tail = match t.class {
            RiskClass::Safe | RiskClass::Rebuildable => {
                format!("selected {}", human(t.selected_bytes))
            }
            RiskClass::Review => "manual choice".to_owned(),
            RiskClass::UserData => "pick by hand".to_owned(),
        };
        writeln!(
            w,
            "  {} {:>6} items {:>10}   {}",
            style.class(t.class, &format!("{:<12}", t.class.as_str())),
            t.count,
            human(t.bytes),
            style.dim(&tail)
        )?;
    }
    writeln!(w)?;
    writeln!(
        w,
        "  {} {} selected by default, frees ~{} → {}",
        style.bold("Can free now:"),
        human(r.totals.selected_bytes),
        human(r.totals.selected_reclaimable_bytes),
        clean_hint(if own_roots { &r.scan_roots } else { &[] }, home)
    )?;

    let inside = inside_worktrees(&r.findings);
    let nested: HashSet<&Path> = inside.values().flatten().map(|f| f.path.as_path()).collect();
    for class in RiskClass::ALL {
        // Hidden by settings.json: counted on one line at the end, as in the totals.
        // What lies in a worktree is listed under it instead.
        let of: Vec<&Finding> = r
            .findings
            .iter()
            .filter(|f| f.class == class && !f.ignored && !nested.contains(f.path.as_path()))
            .collect();
        if of.is_empty() {
            continue;
        }
        writeln!(w)?;
        writeln!(w, "{}", style.class(class, class_title(class)))?;
        let mut by_category: BTreeMap<Category, Vec<&Finding>> = BTreeMap::new();
        for f in &of {
            by_category.entry(f.category).or_default().push(f);
        }
        let mut categories: Vec<(Category, Vec<&Finding>)> = by_category.into_iter().collect();
        categories.sort_by_key(|(_, fs)| std::cmp::Reverse(fs.iter().map(|f| f.bytes).sum::<u64>()));
        for (category, items) in categories {
            let total: u64 = items.iter().map(|f| f.bytes).sum();
            // Its rows are the top-level items: what lies in a worktree is
            // listed under it, and counted here after them.
            let (name, inner) = (style.bold(category.as_str()), inside_them(&items, &inside));
            writeln!(w, "  {name} · {} items · {}{inner}", items.len(), human(total))?;
            rows(w, &items, &inside, 0, home, style)?;
        }
    }

    let ignored: Vec<&Finding> = r.findings.iter().filter(|f| f.ignored).collect();
    if !ignored.is_empty() {
        writeln!(w)?;
        writeln!(
            w,
            "{} {} items ignored by settings.json ({}): switched-off types or under the minimum size.",
            style.dim("note:"),
            ignored.len(),
            human(r.totals.ignored_bytes)
        )?;
        if style.all {
            for f in ignored {
                writeln!(
                    w,
                    "    · {:>10}  {}  {}  {}",
                    human(f.bytes),
                    tilde(&f.path, home),
                    style.dim(&f.rule),
                    style.dim(&status(f, home))
                )?;
            }
        }
    }

    if !r.not_scanned.is_empty() {
        let skipped: Vec<String> = r.not_scanned.iter().map(|p| tilde(p, home)).collect();
        writeln!(w)?;
        writeln!(w, "{} Not scanned: {}", style.dim("note:"), skipped.join(", "))?;
    }

    if !r.large_files.is_empty() {
        writeln!(w)?;
        writeln!(w, "{}", style.bold("LARGE FILES — not covered by any rule, check by hand"))?;
        for f in &r.large_files {
            writeln!(w, "    {:>10}  {}", human(f.bytes), tilde(&f.path, home))?;
        }
    }
    if !r.inaccessible.is_empty() {
        let n = r.inaccessible.len();
        writeln!(w)?;
        let hint = if r.full_disk_access {
            String::new()
        } else {
            " Grant your terminal Full Disk Access (System Settings → Privacy & Security) for a complete scan."
                .to_owned()
        };
        writeln!(w, "{} {n} folder{} could not be read.{hint}", style.dim("note:"), if n == 1 { "" } else { "s" })?;
    }
    Ok(())
}

/// The whole-disk report as a table (`ddc disk`).
pub fn disk(w: &mut impl Write, r: &DiskReport, style: &Style) -> io::Result<()> {
    let home = r.home.path.as_path();
    match &r.container {
        Some(c) => {
            writeln!(w, "{} {} used of {}", style.bold("Startup disk"), human(c.in_use_bytes), human(c.capacity_bytes))?
        }
        None => writeln!(w, "{} (its volumes could not be read)", style.bold("Startup disk"))?,
    }
    if r.cancelled {
        writeln!(w, "{}", style.dim("Stopped: rows marked \"at least\" were not finished."))?;
    }
    writeln!(w)?;
    let row = |w: &mut dyn Write, title: &str, bytes: u64, partial: bool, note: &str| -> io::Result<()> {
        let size = if partial { format!("at least {}", human(bytes)) } else { human(bytes) };
        writeln!(w, "  {title:<28} {size:>20}  {}", style.dim(note))
    };
    let home_note = match r.home.source {
        HomeSource::LastScan => format!("from the scan at {}", r.home.as_of),
        HomeSource::Walk => "measured now".to_owned(),
    };
    row(w, &format!("Home folder {}", tilde(home, home)), r.home.bytes, r.home.partial, &home_note)?;
    // Each nested area right under the one it lies in, when that one is listed.
    let listed = |id: &Option<String>| id.as_ref().is_some_and(|id| r.areas.iter().any(|a| &a.id == id));
    let ordered = r.areas.iter().filter(|a| !listed(&a.parent)).flat_map(|top| {
        std::iter::once(top).chain(r.areas.iter().filter(move |a| a.parent.as_deref() == Some(top.id.as_str())))
    });
    for a in ordered {
        let mut note = a.frees.clone();
        if let Some(n) = a.entries_read.filter(|_| a.partial) {
            note = format!("{} entries read; {note}", count(n));
        }
        if a.unreadable > 0 {
            note = format!("{} unreadable; {note}", count(a.unreadable));
        }
        if a.id == "simulator-runtimes"
            && let Some(s) = r.simulators.as_ref().filter(|s| s.status == SimulatorStatus::Listed)
        {
            let kept = a.bytes.saturating_sub(s.listed_bytes);
            note = if kept > 0 {
                format!("listed runtimes {}; {} more in their folders", human(s.listed_bytes), human(kept))
            } else {
                format!("listed runtimes {}", human(s.listed_bytes))
            };
        }
        let title = if listed(&a.parent) { format!("  {}", a.title) } else { a.title.clone() };
        row(w, &title, a.bytes, a.partial, &note)?;
    }
    let nc = &r.not_counted;
    match nc.bytes {
        Some(b) if nc.inconsistent => writeln!(
            w,
            "  {:<28} {:>20}  {}",
            "Not counted",
            format!("-{}", human(b.unsigned_abs())),
            style
                .dim("more was counted than the volume holds: the home row may be stale, or clones were counted twice")
        )?,
        Some(b) => row(
            w,
            "Not counted",
            b.unsigned_abs(),
            false,
            "the Spotlight index, folders only root can read, APFS metadata",
        )?,
        None => writeln!(w, "  {:<28} {:>20}", "Not counted", "unknown")?,
    }
    for held in &nc.holds {
        let what = match held.kind {
            HeldKind::Cloud => "cloud folder, not read",
            HeldKind::Skip => "in your skips, not read",
        };
        writeln!(w, "    {}  {}", tilde(&held.path, home), style.dim(what))?;
    }
    if nc.unreadable > 0 {
        writeln!(w, "    {}", style.dim(&format!("{} folders could not be read", count(nc.unreadable))))?;
    }

    if let Some(s) = &r.simulators {
        writeln!(w)?;
        writeln!(w, "{}", style.bold("Simulator runtimes"))?;
        if let Some(message) = &s.message {
            writeln!(w, "  {message}")?;
        }
        for rt in &s.runtimes {
            let name = rt.runtime_identifier.as_deref().unwrap_or(&rt.identifier);
            let devices = format!(
                "{} device{}{}",
                rt.devices.len(),
                if rt.devices.len() == 1 { "" } else { "s" },
                if rt.booted { ", one running" } else { "" }
            );
            writeln!(w, "  {name:<48} {:>10}  {}", human(rt.size_bytes.unwrap_or(0)), style.dim(&devices))?;
        }
    }
    if let Some(c) = r.conda.as_ref().filter(|c| !c.installs.is_empty()) {
        writeln!(w)?;
        writeln!(w, "{}", style.bold("Conda"))?;
        let folder = |w: &mut dyn Write, title: &str, f: &ddc_core::disk::CondaFolder| -> io::Result<()> {
            let size = if f.partial { format!("at least {}", human(f.bytes)) } else { human(f.bytes) };
            let mut notes = Vec::new();
            if f.shared_bytes > 0 {
                notes.push(format!("{} shared with other folders", human(f.shared_bytes)));
            }
            if !f.writable {
                notes.push("needs an administrator".to_owned());
            }
            writeln!(w, "  {title:<48} {size:>10}  {}", style.dim(&notes.join("; ")))
        };
        for install in &c.installs {
            writeln!(w, "  {}", escaped(&install.prefix.to_string_lossy()))?;
            if let Some(p) = &install.packages {
                folder(w, "  package cache", p)?;
            }
            for e in &install.environments {
                let name = e.path.file_name().map_or_else(String::new, |n| escaped(&n.to_string_lossy()));
                folder(w, &format!("  {name}"), e)?;
            }
        }
    }
    if let Some(c) = &r.container {
        writeln!(w)?;
        writeln!(w, "{}", style.bold("Volumes"))?;
        for v in c.volumes.iter().filter(|v| v.role != "data") {
            let size = v.used_bytes.map_or_else(|| "not measured".to_owned(), human);
            let note = if v.role == "vm" { "swap; a restart frees it" } else { "" };
            writeln!(w, "  {:<28} {size:>20}  {}", format!("{} ({})", v.name, v.role), style.dim(note))?;
        }
    }
    writeln!(w)?;
    match r.snapshots.count {
        Some(n) => writeln!(w, "Local Time Machine snapshots: {n} (their size needs root to measure)")?,
        None => writeln!(w, "Local Time Machine snapshots: unknown")?,
    }
    Ok(())
}

/// `ddc agent status`: per harness, what runs, its folders and its live
/// sessions (story "Coding agents in the Whole disk view").
pub fn agents(w: &mut impl Write, r: &AgentsReport, home: &Path, style: &Style) -> io::Result<()> {
    if r.harnesses.is_empty() {
        return writeln!(w, "No coding-agent harness keeps folders in {}.", tilde(home, home));
    }
    let size = |bytes: Option<u64>, unreadable: u64| match bytes {
        Some(b) if unreadable > 0 => format!("at least {}", human(b)),
        Some(b) => human(b),
        None => String::new(),
    };
    for (i, h) in r.harnesses.iter().enumerate() {
        if i > 0 {
            writeln!(w)?;
        }
        let mut kinds: Vec<(&str, usize)> = Vec::new();
        for p in &h.running {
            let kind = match p.kind {
                RunningKind::Cli => "CLI",
                RunningKind::AppServer => "app-server",
                RunningKind::App => "app",
                RunningKind::Other => "other",
            };
            match kinds.iter_mut().find(|(k, _)| *k == kind) {
                Some((_, n)) => *n += 1,
                None => kinds.push((kind, 1)),
            }
        }
        let runs = if kinds.is_empty() {
            "not running".to_owned()
        } else {
            let parts: Vec<String> =
                kinds.iter().map(|(k, n)| if *n == 1 { (*k).to_owned() } else { format!("{k} ×{n}") }).collect();
            format!("running: {}", parts.join(", "))
        };
        let unreadable = h.folders.iter().filter(|f| !f.nested).map(|f| f.unreadable).sum();
        writeln!(w, "{:<44} {:>14}  {}", style.bold(&h.name), size(h.bytes, unreadable), style.dim(&runs))?;
        for f in &h.folders {
            let indent = if f.nested { "    " } else { "  " };
            let path = format!("{indent}{}", tilde(&f.path, home));
            let note = if f.skipped {
                "in your skips, not read".to_owned()
            } else if f.unreadable > 0 {
                format!("{} unreadable", count(f.unreadable))
            } else {
                String::new()
            };
            writeln!(w, "{path:<44} {:>14}  {}", size(f.bytes, f.unreadable), style.dim(&note))?;
        }
        let Some(s) = &h.sessions else { continue };
        if s.records == RecordsState::Unknown {
            let why = match s.reason {
                Some(UnknownReason::Skipped) => "they are in your skips",
                Some(UnknownReason::Missing) => "their folder is missing",
                Some(UnknownReason::Unreadable) => "their folder could not be read",
                Some(UnknownReason::Unparsed) | None => "their format is not known",
            };
            writeln!(w, "  {}", style.dim(&format!("ddc cannot read {}'s session records: {why}", h.name)))?;
        }
        for x in s.list.iter().filter(|x| x.state == SessionState::Live) {
            let place = x
                .cwd
                .as_deref()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
                .or_else(|| x.id.clone())
                .map(|p| escaped(&p))
                .unwrap_or_default();
            let note: Vec<String> = [&x.status, &x.name].into_iter().flatten().map(|s| escaped(s)).collect();
            let since = escaped(x.started_at.as_deref().or(x.rollout_time.as_deref()).unwrap_or_default());
            let label = if h.id == ddc_core::agents::HarnessId::Codex { "open in Codex" } else { "live" };
            writeln!(w, "  {label:<12} {place:<30} {}  {}", style.dim(&since), note.join(" · "))?;
        }
        let t = &s.transcripts;
        match (t.skipped, t.count) {
            (true, _) => writeln!(w, "  {}", style.dim("transcripts: in your skips, not read"))?,
            (false, Some(n)) => {
                let bytes = t.bytes.map(|b| format!(", {}", human(b))).unwrap_or_default();
                writeln!(w, "  {}", style.dim(&format!("{} other transcripts{bytes}", count(n))))?;
            }
            (false, None) => {}
        }
        if let Some(t) = &s.temp_root {
            temp_root(w, t, home, style)?;
        }
        if let Some(f) = &s.footprints {
            footprints(w, f, style)?;
        }
    }
    if !r.sizes {
        writeln!(w)?;
        writeln!(w, "{}", style.dim("Sizes were not measured (--no-sizes)."))?;
    }
    Ok(())
}

/// Claude Code's temporary folder outside the home.
fn temp_root(w: &mut impl Write, t: &TempRoot, home: &Path, style: &Style) -> io::Result<()> {
    let path = format!("  temporary folder {}", tilde(&t.path, home));
    let refused = t.refused.map(|why| match why {
        TempRefusal::Symlink => "not read: a symbolic link",
        TempRefusal::NotAFolder => "not read: not a folder",
        TempRefusal::NotOwned => "not read: not your own folder",
    });
    let (bytes, note) = match (refused, t.bytes) {
        _ if t.skipped => (String::new(), "in your skips, not read".to_owned()),
        (Some(why), _) => (String::new(), why.to_owned()),
        (None, Some(b)) if t.unreadable > 0 => {
            (format!("at least {}", human(b)), format!("{} unreadable", count(t.unreadable)))
        }
        (None, Some(b)) => (human(b), String::new()),
        (None, None) => (String::new(), String::new()),
    };
    writeln!(w, "{path:<44} {bytes:>14}  {}", style.dim(&note))
}

/// The sessions by total, largest first: the top ten, then the rest as one
/// line. A live or unknown session's files are only described, never
/// offered; a session that is not live says what its temporary folder
/// holds, and `--json` gives the folder's path.
fn footprints(w: &mut impl Write, list: &[SessionFootprint], style: &Style) -> io::Result<()> {
    if list.is_empty() {
        return Ok(());
    }
    writeln!(w, "  {}", style.dim("sessions, largest first:"))?;
    for f in list.iter().take(TOP_SESSIONS) {
        let state = match f.state {
            FootprintState::Live => "live",
            FootprintState::NotLive => "not live",
            FootprintState::Unknown => "unknown",
        };
        let mut parts: Vec<String> = Vec::new();
        if f.transcript > 0 {
            parts.push(format!("transcript {}", human(f.transcript)));
        }
        if f.checkpoints > 0 {
            parts.push(format!("checkpoints {}", human(f.checkpoints)));
        }
        match f.state {
            FootprintState::NotLive if f.temp > 0 => {
                parts.push(format!("{} in its temporary folder", human(f.temp)));
            }
            _ if f.temp > 0 => parts.push(format!("temporary {}", human(f.temp))),
            _ => {}
        }
        let id: String = f.id.chars().take(8).collect();
        let project = escaped(f.project.as_deref().unwrap_or("?"));
        writeln!(w, "    {state:<9} {id:<8}  {project:<24} {:>12}  {}", human(f.total), style.dim(&parts.join(" · ")))?;
    }
    let rest = &list[list.len().min(TOP_SESSIONS)..];
    if !rest.is_empty() {
        let bytes: u64 = rest.iter().map(|f| f.total).sum();
        writeln!(w, "    {}", style.dim(&format!("{} more, {}", count(rest.len() as u64), human(bytes))))?;
    }
    Ok(())
}

/// `ddc disk free`: one block per action, in list order.
pub fn free(w: &mut impl Write, r: &FreeReport, home: &Path, style: &Style) -> io::Result<()> {
    for (i, a) in r.actions.iter().enumerate() {
        if i > 0 {
            writeln!(w)?;
        }
        free_action(w, a, style)?;
    }
    if let Some(journal) = &r.journal {
        writeln!(w, "\n{}", style.dim(&format!("Journal: {}", tilde(journal, home))))?;
    }
    for (name, edit) in [("last-scan.json", r.last_scan), ("last-disk.json", r.last_disk)] {
        let note = match edit {
            ReportEdit::Skipped => format!("{name} was not edited: absent, or of another version or other folders."),
            ReportEdit::Failed => format!("{name} could not be saved."),
            ReportEdit::Edited | ReportEdit::Unchanged => continue,
        };
        writeln!(w, "{}", style.dim(&note))?;
    }
    Ok(())
}

/// `Simulator runtime <id>`, `Homebrew cleanup`, `Conda environment <path>`,
/// `Conda package cache of <prefix>`, `Homebrew formula <full_name>`.
pub fn action_title(action: ActionId, identifier: Option<&str>) -> String {
    match (action, identifier) {
        (ActionId::SimctlRuntimeDelete, Some(id)) => format!("Simulator runtime {id}"),
        (ActionId::SimctlRuntimeDelete, None) => "Simulator runtime".to_owned(),
        (ActionId::BrewCleanup, _) => "Homebrew cleanup".to_owned(),
        (ActionId::CondaEnvRemove, Some(path)) => format!("Conda environment {}", escaped(path)),
        (ActionId::CondaEnvRemove, None) => "Conda environment".to_owned(),
        (ActionId::CondaClean, Some(prefix)) => format!("Conda package cache of {}", escaped(prefix)),
        (ActionId::CondaClean, None) => "Conda package cache".to_owned(),
        (ActionId::BrewUninstall, Some(name)) => format!("Homebrew formula {}", escaped(name)),
        (ActionId::BrewUninstall, None) => "Homebrew formula".to_owned(),
        (ActionId::BrewUninstallOrphans, _) => "Homebrew formulae needed by nothing".to_owned(),
    }
}

fn free_action(w: &mut impl Write, a: &ActionReport, style: &Style) -> io::Result<()> {
    let freed = match (a.bytes_before, a.bytes_after) {
        (Some(before), Some(after)) => Some(before.saturating_sub(after)),
        _ => None,
    };
    let result = match &a.result {
        ActionResult::Freed => freed.map_or_else(|| "done".to_owned(), |b| format!("freed {}", human(b))),
        ActionResult::RemovedNotFreed if a.action == ActionId::CondaEnvRemove => {
            "conda finished, and the environment is still there: nothing freed".to_owned()
        }
        ActionResult::RemovedNotFreed if a.area == "homebrew" => {
            "brew finished, and a keg is still in the Cellar: not all of it was freed".to_owned()
        }
        ActionResult::RemovedNotFreed => {
            format!("runtime removed, {} not freed", a.bytes_after.map_or_else(|| "its space".to_owned(), human))
        }
        ActionResult::StillDeleting => "still deleting: CoreSimulator may finish it later".to_owned(),
        ActionResult::Refused { reason } => format!("refused: {}", refusal(*reason)),
        ActionResult::Failed { .. } => "failed".to_owned(),
        ActionResult::WouldRun => "would run".to_owned(),
        ActionResult::NotRun => "not run: stopped".to_owned(),
    };
    writeln!(w, "{}  {result}", style.bold(&action_title(a.action, a.identifier.as_deref())))?;
    match &a.result {
        ActionResult::Failed { needs_admin } => {
            let lead = if *needs_admin {
                "It needs an administrator. To run it yourself, in Terminal:"
            } else {
                "The tool failed. To run it yourself, in Terminal:"
            };
            writeln!(w, "  {lead}\n    {}", a.terminal_command)?;
        }
        ActionResult::WouldRun => {
            writeln!(w, "  {}", a.terminal_command)?;
            if let Some(p) = &a.preview {
                // A formula's preview lines are its kegs' paths, listed below.
                for line in p.lines.iter().filter(|_| a.kegs.is_empty()) {
                    writeln!(w, "    {}", style.dim(line))?;
                }
                if let Some(b) = p.estimate_bytes {
                    writeln!(w, "  frees about {}", human(b))?;
                }
            }
        }
        // A package removal names its formulae in its command.
        _ if matches!(a.action, ActionId::BrewUninstall | ActionId::BrewUninstallOrphans) => {
            writeln!(w, "  {}", style.dim(&a.terminal_command))?;
        }
        _ => {}
    }
    for k in &a.kegs {
        let size = k.bytes.map_or_else(|| "not measured".to_owned(), human);
        let keg = format!("{} {}  {}", escaped(&k.formula), escaped(&k.version), escaped(&k.path.to_string_lossy()));
        writeln!(w, "    {keg}  {size}")?;
    }
    for n in &a.needed_by {
        let casks = n.casks.iter().map(|c| format!("cask {}", escaped(c)));
        let by: Vec<String> = n.formulae.iter().map(|f| escaped(f)).chain(casks).collect();
        writeln!(w, "  {} is needed by {}", escaped(&n.formula), by.join(", "))?;
    }
    if !a.output.is_empty() && matches!(a.result, ActionResult::Failed { .. }) {
        for line in a.output.lines() {
            writeln!(w, "    {}", style.dim(line))?;
        }
    }
    Ok(())
}

/// `ddc disk brew-packages`: each formula installed on request, largest
/// first, its kegs when it has more than one, what only it holds, and why
/// a removal is not offered.
pub fn brew_packages(w: &mut impl Write, r: &BrewPackages, home: &Path, style: &Style) -> io::Result<()> {
    let size = |bytes: Option<u64>| bytes.map_or_else(|| "not measured".to_owned(), human);
    let names = |list: &[String]| list.iter().map(|n| escaped(n)).collect::<Vec<_>>().join(", ");
    if r.formulae.is_empty() {
        writeln!(w, "No formula installed on request in {}.", tilde(&r.cellar, home))?;
    }
    for p in &r.formulae {
        let note = match p.not_offered {
            None => String::new(),
            Some(NotOffered::Needed) => {
                let casks: Vec<String> = p.needed_by_casks.iter().map(|c| format!("cask {c}")).collect();
                format!("needed by {}", names(&[p.needed_by.clone(), casks].concat()))
            }
            Some(NotOffered::Pinned) => "pinned".to_owned(),
            Some(NotOffered::Service) => brew_service(p.service).to_owned(),
            Some(NotOffered::ServicesUnreadable) => "its service state could not be read".to_owned(),
            Some(NotOffered::FieldMissing) => "brew listed a formula without a field ddc needs".to_owned(),
        };
        writeln!(w, "{:<40} {:>14}  {}", style.bold(&escaped(&p.full_name)), size(p.bytes), style.dim(&note))?;
        if p.kegs.len() > 1 {
            for k in &p.kegs {
                writeln!(w, "  {:<38} {:>14}", escaped(&k.version), size(k.bytes))?;
            }
        }
        if !p.would_orphan.is_empty() {
            let alone = format!("only it needs {} ({})", names(&p.would_orphan), size(p.would_orphan_bytes));
            writeln!(w, "  {}", style.dim(&alone))?;
        }
        if !p.kept_pinned.is_empty() {
            writeln!(w, "  {}", style.dim(&format!("kept, pinned: {}", names(&p.kept_pinned))))?;
        }
    }
    if !r.orphans.is_empty() {
        writeln!(w, "\n{}", style.dim(&format!("Needed by nothing now: {}.", names(&r.orphans))))?;
    }
    if r.services == ServicesState::NotKnown {
        writeln!(
            w,
            "{}",
            style.dim("This brew has no services command: whether a service is registered is not known.")
        )?;
    }
    if !r.incomplete.is_empty() {
        let missing = format!("brew listed {} without a field ddc needs: nothing is offered.", names(&r.incomplete));
        writeln!(w, "{}", style.dim(&missing))?;
    }
    Ok(())
}

/// `ddc agent footprint`: the main checkout, then each worktree with its
/// state and where it counts, the artifacts inside each, and the totals.
pub fn footprint(w: &mut impl Write, r: &FootprintReport, home: &Path, style: &Style) -> io::Result<()> {
    let class_word = |c: RiskClass| match c {
        RiskClass::Safe => "safe",
        RiskClass::Rebuildable => "rebuildable",
        RiskClass::Review => "review",
        RiskClass::UserData => "user data",
    };
    if let Some(why) = &r.listing_error {
        writeln!(
            w,
            "{}",
            style.dim(&format!("git could not list the worktrees ({}); the main checkout only.", escaped(why)))
        )?;
    }
    for c in &r.checkouts {
        let state = footprint_state(c);
        let label = match c.kind {
            FootprintKind::Main => "main checkout (its findings)".to_owned(),
            FootprintKind::Linked | FootprintKind::Orphan => state,
        };
        writeln!(w, "{:<56} {:>14}  {}", style.bold(&tilde(&c.path, home)), human(c.bytes), style.dim(&label))?;
        for f in &c.findings {
            let shown =
                f.path.strip_prefix(&c.path).map_or_else(|_| tilde(&f.path, home), |rel| rel.display().to_string());
            let mark = if f.selected { "selected" } else { "" };
            writeln!(
                w,
                "  {:<54} {:>14}  {} {}",
                escaped(&shown),
                human(f.bytes),
                style.class(f.class, class_word(f.class)),
                style.dim(mark)
            )?;
        }
    }
    for n in &r.not_scanned {
        let why = match n.reason {
            NotScannedReason::Skipped => "under a skip",
            NotScannedReason::Missing => "git lists it, but the folder is gone",
            NotScannedReason::OtherFilesystem => "on another filesystem",
            NotScannedReason::NotRecognized => "not read as a worktree",
        };
        writeln!(w, "{:<56} {:>14}  {}", tilde(&n.path, home), "not scanned", style.dim(why))?;
    }
    let t = &r.totals;
    writeln!(
        w,
        "\nSelected now: {} (safe {}, rebuildable {}). After the sessions end: about {}. Held back: {}.",
        human(t.reclaimable_now.bytes),
        human(t.reclaimable_now.safe),
        human(t.reclaimable_now.rebuildable),
        human(t.reclaimable_after_sessions),
        human(t.held_back)
    )?;
    // Only for what lies under the main checkout: `ddc clean <main>` reaches nothing else.
    let under_main = r.checkouts.iter().any(|c| {
        c.path.starts_with(&r.main)
            && (c.bucket == Some(Bucket::Now) || c.findings.iter().any(|f| f.selected && !f.ignored))
    });
    if under_main && let Some(command) = clean_command(&r.clean_roots, home) {
        writeln!(w, "Run `{command}` to review and remove what is selected.")?;
    }
    if !r.outside_main.is_empty() {
        let paths: Vec<String> = r.outside_main.iter().map(|p| tilde(p, home)).collect();
        writeln!(w, "Outside the main checkout, review in the app's Review window: {}.", paths.join(", "))?;
    }
    match (&r.harness, r.harness_reason) {
        (Some(h), _) if !h.findings.is_empty() => {
            let bytes: u64 = h.findings.iter().map(|f| f.bytes).sum();
            writeln!(w, "{}", style.dim(&format!("Coding agents' own data at the last scan: {}.", human(bytes))))?;
        }
        _ => {}
    }
    Ok(())
}

/// A worktree's state words and where it counts: `clean · unlocked · idle
/// 3d · selected`, `dirty · held back`.
fn footprint_state(c: &CheckoutEntry) -> String {
    let mut words = Vec::new();
    if c.kind == FootprintKind::Orphan {
        words.push("orphan".to_owned());
    }
    if let Some(s) = &c.worktree {
        match s.clean {
            Some(true) => words.push("clean".to_owned()),
            Some(false) => words.push("dirty".to_owned()),
            None => {}
        }
        if let Some(ignored) = &s.ignored_content
            && let Some(first) = ignored.paths.first()
        {
            let first = first.strip_prefix(&c.path).unwrap_or(first).display().to_string();
            let more = if ignored.count > 1 { format!(" +{}", ignored.count - 1) } else { String::new() };
            words.push(format!("ignored: {}{more}", escaped(&first)));
        }
        if s.submodules == Some(true) {
            words.push("submodules".to_owned());
        }
        if let Some(n) = s.own_commits.filter(|n| *n > 0) {
            words.push(format!("own commits: {n}"));
        }
        match (s.locked, c.lock) {
            (Some(true), Some(Lock { pid: Some(pid), live: Some(true) })) => {
                words.push(format!("locked by a live session (pid {pid})"))
            }
            (Some(true), _) => words.push("locked".to_owned()),
            (Some(false), _) => words.push("unlocked".to_owned()),
            (None, _) => {}
        }
        if s.in_use == Some(true) {
            words.push("in use".to_owned());
        }
        if let Some(days) = s.idle_days {
            words.push(format!("idle {days}d"));
        }
    }
    words.push(match (c.bucket, c.reason) {
        (Some(Bucket::Now), _) => "selected".to_owned(),
        (Some(Bucket::AfterSessions), _) => "after its session ends".to_owned(),
        (_, Some(reason)) => format!("held back: {}", held_reason(reason)),
        _ => "held back".to_owned(),
    });
    words.join(" · ")
}

fn held_reason(reason: HeldReason) -> &'static str {
    match reason {
        HeldReason::Submodules => "a submodule is initialized",
        HeldReason::Dirty => "uncommitted changes",
        HeldReason::IgnoredContent => "ignored content of its own",
        HeldReason::OwnCommits => "commits of its own",
        HeldReason::StaleLock => "a lock no session holds (git worktree unlock)",
        HeldReason::ProbeUnknown => "git could not tell its state",
        HeldReason::NotLinked => "copied or moved by hand",
        HeldReason::NotAHarness => "made by a tool ddc does not select from",
        HeldReason::Orphan => "its repository entry is gone",
        HeldReason::Yours => "your own worktree",
        HeldReason::Immutable => "a file in it is immutable",
        HeldReason::ForeignOwner => "another account owns it",
        HeldReason::Kept => "protected by an exclude or .ddc-keep",
        HeldReason::HiddenBySettings => "under min_size",
        HeldReason::ActivityUnknown => "nothing tells when it was last used",
        HeldReason::RuleDisabled => "git.worktree is switched off",
    }
}

/// `ddc disk docker`: each kind of record with its count and size, what a
/// clean would free, and what it leaves; or why docker did not say.
pub fn docker(w: &mut impl Write, c: &DockerContents, style: &Style) -> io::Result<()> {
    let size = |g: &Group| g.bytes.map_or_else(|| "size not known".to_owned(), human);
    match &c.result {
        DockerResult::Listed { images, containers, volumes, build_cache, estimate } => {
            writeln!(w, "{}", style.bold("Docker Desktop's disk image holds:"))?;
            let rows = [
                ("images", images, "unique to each image"),
                ("containers", containers, "their writable layers"),
                ("volumes", volumes, ""),
                ("build cache", build_cache, "not shared with images"),
            ];
            for (name, g, note) in rows {
                writeln!(w, "  {name:<12} {:>6}  {:>14}  {}", g.count, size(g), style.dim(note))?;
            }
            let frees =
                estimate.bytes.map_or_else(|| "an amount not known".to_owned(), |b| format!("about {}", human(b)));
            let dangling = estimate.dangling_images.map_or_else(|| "not known".to_owned(), human);
            writeln!(
                w,
                "\nA clean frees {frees}: unused build cache {}, dangling images {dangling}.",
                human(estimate.build_cache)
            )?;
            writeln!(w, "{}", style.dim("It leaves these; each is yours to run:"))?;
            for (what, cmd) in [
                ("unused tagged images", "docker image prune -a"),
                ("stopped containers", "docker container prune"),
                ("volumes, with their data", "docker volume prune"),
                ("the rest of the build cache", "docker builder prune --all"),
            ] {
                writeln!(w, "  {what:<28} {}", style.dim(cmd))?;
            }
            writeln!(
                w,
                "{}",
                style.dim(
                    "These sizes leave out shared layers, and the disk image keeps space it freed until \
                     Docker Desktop shrinks it: they do not add up to its folder's size."
                )
            )?;
        }
        DockerResult::NotInstalled => writeln!(w, "Docker's command-line tool was not found.")?,
        DockerResult::NotReady { detail } => {
            if detail == ddc_core::rules::DOCKER_DOWN {
                writeln!(w, "{detail}.")?;
            } else {
                writeln!(w, "Docker did not answer: {}.", escaped(detail.trim_end_matches('.')))?;
            }
            if let Some(advice) = docker_advice(detail) {
                writeln!(w, "{}", style.dim(advice))?;
            }
        }
        DockerResult::Failed { detail } => writeln!(w, "docker system df failed: {}", escaped(detail))?,
        DockerResult::TimedOut { seconds } => {
            writeln!(w, "docker system df did not finish within {seconds} seconds.")?;
        }
        DockerResult::Unreadable { detail } => {
            writeln!(w, "docker system df printed what ddc cannot read: {}", escaped(detail))?
        }
    }
    Ok(())
}

/// What to do when the probe said `detail` (story "VM images: docker prune
/// runs only while Docker Desktop answers").
fn docker_advice(detail: &str) -> Option<&'static str> {
    if detail == ddc_core::rules::DOCKER_DOWN {
        Some("Start Docker Desktop to see what is inside.")
    } else if detail == ddc_core::rules::DOCKER_READY.no_context {
        Some("If you no longer use Docker Desktop, its folder is yours to delete.")
    } else {
        None
    }
}

/// Why a formula's service keeps its removal from being offered.
pub fn brew_service(s: Service) -> &'static str {
    match s {
        Service::Running => "a service of yours is loaded for it; stop it with brew services first",
        Service::Root => "a root service is registered for it; its state is not known",
        Service::Registered => {
            "a service of yours is registered for it but not running; remove it with brew services first"
        }
        Service::Unreadable => "its service state could not be read",
        Service::NotKnown => "whether a service is registered for it is not known",
        Service::None => "no service of yours registered with launchd",
    }
}

pub fn refusal(r: Refusal) -> &'static str {
    match r {
        Refusal::UnknownRuntime => "no runtime with this identifier is listed",
        Refusal::NotUnderAssets => "its mount path is not under /System/Library/AssetsV2",
        Refusal::NotDeletable => "simctl lists it as not deletable",
        Refusal::Deleting => "it is already being deleted",
        Refusal::NoRuntimeIdentifier => "it has no runtime identifier, so its devices cannot be checked",
        Refusal::Booted => "a simulator on it is running or not yet shut down",
        Refusal::DevicesUnknown => "its devices could not be listed",
        Refusal::NoAssetFolder => "its folder under AssetsV2 is not known, so its size cannot be measured",
        Refusal::NoDeveloperTools => "Xcode's developer tools are not installed",
        Refusal::FirstLaunchPending => "Xcode has not finished its first launch",
        Refusal::SimctlFailed => "simctl failed",
        Refusal::BrewMissing => "/opt/homebrew/bin/brew is missing or did not answer",
        Refusal::PrefixMismatch => "brew --prefix is not /opt/homebrew",
        Refusal::CellarOutside => "brew --cellar is not inside /opt/homebrew",
        Refusal::CacheMismatch => "brew --cache is not ~/Library/Caches/Homebrew",
        Refusal::CondaInHome => "it is in the home folder: the Review window cleans it",
        Refusal::CondaNotInOpt => {
            "it is not an environment or the install of /opt/miniconda3, /opt/anaconda3 or /opt/miniforge3"
        }
        Refusal::CondaMissing => "the install's conda is missing or did not answer",
        Refusal::CondaRootMismatch => "conda's root_prefix is not the install's folder",
        Refusal::CondaBase => "it is the install's base environment",
        Refusal::CondaNotInEnvsDirs => "its folder is not in conda's envs_dirs",
        Refusal::CondaNotAnEnvironment => "it holds no conda-meta/history",
        Refusal::CondaPackagesNotListed => "the install's pkgs folder is not in conda's pkgs_dirs",
        Refusal::CondaNotWritable => "your user cannot write it: conda needs an administrator",
        Refusal::CondaInUse => "a running process uses the environment",
        Refusal::CondaSoftlinks => "an install links packages into its environments by symlink",
        Refusal::CondaNotScoped => "conda's package folders, set in a .condarc, cannot be limited to this one",
        Refusal::BrewNotInstalled => "brew no longer lists it as installed",
        Refusal::BrewPinned => "it is pinned: brew unpin it first",
        Refusal::BrewKegsChanged => "its installed versions changed since you confirmed",
        Refusal::BrewNeeded => "something installed still needs it",
        Refusal::BrewNotOrphan => "it was installed on request, so it is no longer unneeded",
        Refusal::BrewServiceRunning => brew_service(Service::Running),
        Refusal::BrewServiceRoot => brew_service(Service::Root),
        Refusal::BrewServiceRegistered => brew_service(Service::Registered),
        Refusal::BrewFieldMissing => "brew listed a formula without a field ddc needs",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `ddc space`, `ddc changes` and `ddc scan` say an age the same way, with
    /// the app's thresholds.
    #[test]
    fn an_age_follows_the_apps_wording_and_a_volume_name_reads_mid_sentence() {
        const NOW: u64 = 1_793_700_000;
        let at = |secs: u64| {
            humantime::format_rfc3339_seconds(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)).to_string()
        };
        let ago = |secs: u64| age(&at(NOW - secs), &at(NOW));
        assert_eq!(ago(59).as_deref(), Some("just now"));
        assert_eq!(ago(60).as_deref(), Some("1 min ago"));
        assert_eq!(ago(3599).as_deref(), Some("59 min ago"));
        assert_eq!(ago(3600).as_deref(), Some("1 h ago"));
        assert_eq!(ago(48 * 3600 - 1).as_deref(), Some("47 h ago"));
        assert_eq!(ago(48 * 3600).as_deref(), Some("2 d ago"));
        assert_eq!(age(&at(NOW + 600), &at(NOW)).as_deref(), Some("just now"), "a baseline from the future");
        assert_eq!(age("yesterday", &at(NOW)), None);
        assert_eq!(mid_sentence("The startup disk"), "the startup disk");
        assert_eq!(mid_sentence("Theatre"), "Theatre");
    }

    #[test]
    fn the_clean_command_names_the_paths_the_scan_was_given() {
        let home = Path::new("/h");
        let cmd = |roots: &[&str]| clean_command(&roots.iter().map(PathBuf::from).collect::<Vec<_>>(), home);
        assert_eq!(cmd(&[]).as_deref(), Some("ddc clean"));
        assert_eq!(cmd(&["/h/Projects"]).as_deref(), Some("ddc clean ~/Projects"));
        assert_eq!(cmd(&["/h"]).as_deref(), Some("ddc clean ~"));
        assert_eq!(cmd(&["/h/My Code", "/Volumes/it's"]).as_deref(), Some(r"ddc clean ~/'My Code' '/Volumes/it'\''s'"));
        assert_eq!(cmd(&["/h/-rf"]).as_deref(), Some("ddc clean ~/-rf"), "a leading dash follows `~/`");
        assert_eq!(cmd(&["/h/a\\b"]).as_deref(), Some(r"ddc clean ~/'a\b'"));
        // A newline and a tab stay literal inside the quotes.
        assert_eq!(cmd(&["/h/a\nb\tc"]).as_deref(), Some("ddc clean ~/'a\nb\tc'"));
        // Another control character or a bidi override: no command.
        assert_eq!(cmd(&["/h/a\u{1b}[2J"]), None);
        assert_eq!(cmd(&["/h/cod\u{202e}fdp."]), None);
        assert!(clean_hint(&[PathBuf::from("/h/a\u{1b}[2J")], home).contains("cannot be pasted"));
    }

    /// Whether there is no `/bin/sh` here (Windows), after printing why
    /// `test` skips.
    fn without_sh(test: &str) -> bool {
        let missing = !Path::new("/bin/sh").exists();
        if missing {
            eprintln!("skipped {test}: no /bin/sh to paste into");
        }
        missing
    }

    /// The suggested command, pasted into the system's own `sh`, names the
    /// same path: each word is read back by `printf '%s\0'`. Where no
    /// `/bin/sh` exists (Windows) there is nothing to paste into.
    #[test]
    fn the_clean_command_pasted_into_sh_names_the_same_paths() {
        if without_sh("the_clean_command_pasted_into_sh_names_the_same_paths") {
            return;
        }
        let home = Path::new("/h");
        for name in ["My Code", "it's", "a\nb", "tab\there", "back\\slash", "-rf", "*", "$HOME", "`id`", "ü"] {
            let root = home.join(name);
            let command = clean_command(std::slice::from_ref(&root), home).expect(name);
            let words = command.strip_prefix("ddc clean ").unwrap();
            let out = std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(format!("printf '%s\\0' {words}"))
                .env("HOME", home)
                .output()
                .unwrap();
            assert!(out.status.success(), "{name:?}: {out:?}");
            assert_eq!(out.stdout, [root.as_os_str().as_encoded_bytes(), b"\0"].concat(), "{name:?}: {command}");
        }
    }

    #[test]
    fn control_characters_are_escaped() {
        let p = Path::new("/h/evil\u{1b}[2J\u{1b}[31mX\n/__pycache__");
        let shown = tilde(p, Path::new("/h"));
        assert!(!shown.chars().any(char::is_control), "{shown:?}");
        assert!(shown.starts_with("~/evil\\u{1b}[2J"));
    }

    #[test]
    fn text_a_harness_wrote_is_escaped() {
        let r: AgentsReport = serde_json::from_str(
            r#"{"schema_version":1,"ddc_version":"0","generated_at":"x","duration_ms":0,"sizes":false,"harnesses":[
              {"id":"claude-code","name":"Claude Code","running":[],"folders":[{"path":"/h/.claude","nested":false,"skipped":false,"unreadable":0}],
               "sessions":{"records":"readable","list":[{"state":"live","cwd":"/h/cod\u202efdp.exe","status":"\u001b[2Jbusy","name":"n\u001b[31m"}],
                 "transcripts":{"skipped":false}}}]}"#,
        )
        .unwrap();
        let mut out = Vec::new();
        agents(&mut out, &r, Path::new("/h"), &Style { color: false, all: false }).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(!text.chars().any(|c| (c.is_control() && c != '\n') || bidi(c)), "{text:?}");
        assert!(text.contains("cod\\u{202e}fdp.exe") && text.contains("\\u{1b}[2Jbusy"), "{text}");
    }

    const NO_HINT: &str = r#","hint":"""#;

    /// A finding as `--json` writes it; `extra` adds its `hint` and the rest.
    fn finding(path: &str, rule: &str, class: &str, selected: bool, extra: &str) -> Finding {
        serde_json::from_str(&format!(
            r#"{{"path":"{path}","rule":"{rule}","category":"project-artifact","class":"{class}","rule_class":"{class}",
               "removal":"delete","bytes":1048576,"shared_bytes":0,"files":1,"dirs":1,"incomplete":false,
               "selected":{selected},"notes":[],"dev":1,"ino":1{extra}}}"#
        ))
        .unwrap()
    }

    /// `extra` for a worktree of `/h/p` whose hint is `hint`.
    fn worktree_of_p(hint: &str, harness: &str, linked: bool) -> String {
        format!(
            r#","hint":{},"project":{{"root":"/h/p"}},"origin":{{"harness":"{harness}","kind":"worktree"}},"worktree":{{"linked":{linked},"probe":"known"}}"#,
            serde_json::to_string(hint).unwrap()
        )
    }

    fn report_of(findings: Vec<Finding>) -> ScanReport {
        let mut r: ScanReport = serde_json::from_str(
            r#"{"schema_version":1,"ddc_version":"0","platform":"linux","capabilities":[],"generated_at":"x",
               "duration_ms":0,"home":"/h","scan_roots":["/h"],"full_disk_access":true,"min_age_days":14,
               "totals":{"scanned_bytes":0,"files":0,"dirs":0,"unowned_bytes":0,"classes":[],"selected_bytes":0,
                 "selected_reclaimable_bytes":0},
               "findings":[],"large_files":[],"inaccessible":[],"errors_total":0,"skipped":[],"skipped_total":0,
               "cancelled":false}"#,
        )
        .unwrap();
        r.findings = findings;
        r
    }

    fn rendered(r: &ScanReport, all: bool) -> String {
        let mut out = Vec::new();
        report(&mut out, r, &Style { color: false, all }, false).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_worktree_lists_what_lies_in_it_under_it_and_names_its_project_and_reason() {
        let home = Path::new("/h");
        let kept = worktree::kept_hint(Path::new("/h/p"), "it has uncommitted changes or untracked files");
        let findings = vec![
            finding(
                "/h/p/.claude/worktrees/x",
                "git.worktree",
                "review",
                false,
                &worktree_of_p(&kept, "claude-code", true),
            ),
            finding("/h/p/.claude/worktrees/x/.venv", "python.venv", "rebuildable", true, NO_HINT),
            finding("/h/p/.claude/worktrees/x/web/node_modules", "node.modules", "rebuildable", true, NO_HINT),
            finding("/h/p/node_modules", "node.modules", "rebuildable", true, NO_HINT),
        ];
        let inside = inside_worktrees(&findings);
        let nested: Vec<&Path> = inside.values().flatten().map(|f| f.path.as_path()).collect();
        assert_eq!(
            nested,
            [Path::new("/h/p/.claude/worktrees/x/.venv"), Path::new("/h/p/.claude/worktrees/x/web/node_modules")]
        );
        let mut out = Vec::new();
        finding_rows(&mut out, &findings[0], &inside, 0, home, &Style { color: false, all: false }).unwrap();
        let text = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(lines[0].starts_with("    · ") && lines[0].contains("~/p/.claude/worktrees/x  git.worktree"), "{text}");
        assert!(lines[0].ends_with("worktree of ~/p · it has uncommitted changes or untracked files"), "{text}");
        assert!(lines[1].starts_with("      ✓ ") && lines[1].contains("  .venv  python.venv  rebuildable"), "{text}");
        assert!(lines[2].contains("  web/node_modules  node.modules  rebuildable"), "{text}");
    }

    #[test]
    fn a_worktree_not_selected_says_why_and_a_selected_one_does_not() {
        let main = Path::new("/h/p");
        let linked = |harness, linked| {
            worktree::hint(&worktree::Recognized::Linked(worktree::Linked {
                gitdir: main.join(".git/worktrees/w"),
                main: main.to_path_buf(),
                harness,
                linked,
            }))
        };
        let mine = worktree_of_p(&linked(OriginHarness::Git, true), "git", true);
        let moved = worktree_of_p(&linked(OriginHarness::ClaudeCode, false), "claude-code", false);
        let evil_hint = worktree::kept_hint(main, "it holds ignored content no artifact accounts for: a\u{1b}[2J");
        let evil = worktree_of_p(&evil_hint, "claude-code", true);
        let picked = worktree_of_p(&linked(OriginHarness::ClaudeCode, true), "claude-code", true);
        let orphan = worktree::hint(&worktree::Recognized::Orphan { harness: OriginHarness::ClaudeCode });
        let orphan = format!(r#","hint":{}"#, serde_json::to_string(&orphan).unwrap());
        let reason = |rule: &str, selected: bool, extra: &str| {
            worktree_reason(&finding("/h/w", rule, "review", selected, extra))
        };
        assert_eq!(reason("git.worktree", false, &mine).as_deref(), Some("yours: list it to remove it"));
        assert_eq!(
            reason("git.worktree", false, &moved).as_deref(),
            Some("moved or copied: git worktree repair relinks it")
        );
        let gone = reason("git.worktree-orphan", false, &orphan).unwrap();
        assert!(gone.contains("not mounted") && gone.ends_with("git worktree repair"), "both causes: {gone}");
        let shown = reason("git.worktree", false, &evil).unwrap();
        assert!(!shown.chars().any(char::is_control) && shown.ends_with("a\\u{1b}[2J"), "{shown}");
        assert_eq!(reason("git.worktree", true, &picked), None);
        assert_eq!(reason("git.worktree", false, &picked), None, "no reason in its hint");
        assert_eq!(reason("python.venv", false, NO_HINT), None);
    }

    /// Every finding inside a worktree is shown once, under it, or counted
    /// in the line for the rows past the cut, never lost with a cut worktree.
    #[test]
    fn what_lies_in_a_worktree_past_the_cut_is_counted_not_lost() {
        let mut findings = Vec::new();
        for i in 0..10 {
            let kept = worktree::kept_hint(Path::new("/h/p"), "it has uncommitted changes or untracked files");
            let wt = format!("/h/p/.claude/worktrees/w{i}");
            findings.push(finding(&wt, "git.worktree", "review", false, &worktree_of_p(&kept, "claude-code", true)));
            findings.push(finding(&format!("{wt}/.venv"), "python.venv", "rebuildable", true, NO_HINT));
        }
        for j in 0..10 {
            findings.push(finding(
                &format!("/h/p/.claude/worktrees/w0/pkg{j}/node_modules"),
                "node.modules",
                "rebuildable",
                true,
                NO_HINT,
            ));
        }
        let r = report_of(findings);

        let text = rendered(&r, false);
        assert!(!text.contains("REBUILDABLE"), "nothing inside a worktree is listed in its own group: {text}");
        // The header counts the ten worktrees, then the twenty findings inside them.
        assert!(text.contains("  project-artifact · 10 items · 10.0 MiB, and 20 inside them, 20.0 MiB\n"), "{text}");
        assert_eq!(text.matches("git.worktree").count(), 8, "{text}");
        assert!(text.contains("    … 2 more, 2.0 MiB, and 2 inside them, 2.0 MiB (--all shows them)"), "{text}");
        // w0 holds 11: its .venv and ten node_modules, cut at eight with the rest counted.
        assert_eq!(text.matches("node.modules").count(), 7, "{text}");
        assert!(text.contains("      … 3 more, 3.0 MiB (--all shows them)"), "{text}");

        let all = rendered(&r, true);
        assert_eq!(all.matches("git.worktree").count(), 10, "{all}");
        assert_eq!(all.matches("python.venv").count(), 10, "{all}");
        assert_eq!(all.matches("node.modules").count(), 10, "{all}");
        assert!(!all.contains("more,"), "{all}");
    }

    #[test]
    fn colors_only_on_a_terminal_without_no_color() {
        assert!(use_color(true, false));
        assert!(!use_color(true, true));
        assert!(!use_color(false, false));
    }
}
