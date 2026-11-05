//! `ddc changes`: what grew between the last two scans (story "ddc changes:
//! what grew between the last two scans"; ADR "Menu bar app: Swift shell
//! over the ddc CLI", §6 item 15). It reads `previous-scan.json`,
//! `last-scan.json` and the action journal; no walk, no lock, no write. The
//! comparison is the core's (`ddc_core::changes`); this prints it.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;
use ddc_core::changes::{
    self, ChangesReport, Group, HomeReason, Item, ItemKind, Loaded, NotReadKind, Reason, cloud_roots,
};
use ddc_core::env::Env;
use ddc_core::model::{OriginHarness, ScanReport};

use crate::cli::ChangesArgs;
use crate::notify::volume_name;
use crate::render::{age, mid_sentence, tilde};
use crate::size::human;

/// The text keeps a change of at least this much, or one of the [`BIGGEST`].
const SHOWN_MIN: u64 = 100 << 20;
const BIGGEST: usize = 20;
/// The groups `ddc scan`'s block names, the biggest growth first.
const BLOCK_GROUPS: usize = 3;

pub fn run(args: &ChangesArgs) -> anyhow::Result<ExitCode> {
    let env = Env::from_process(args.data_dir.clone())?;
    run_with(args, &env, &mut io::stdout().lock())
}

/// Exit 0 when it compared or said why not; an error, which the caller
/// prints as a `ddc:` line, when a report or the journal is there but
/// cannot be read.
pub fn run_with(args: &ChangesArgs, env: &Env, out: &mut dyn Write) -> anyhow::Result<ExitCode> {
    let loaded = changes::load(env).context("cannot read the scan reports")?;
    let journal = changes::read_journal(env)
        .with_context(|| format!("cannot read {}", env.log_dir().join("actions.jsonl").display()))?;
    let report = changes::compare_loaded(&loaded, &journal, &cloud_roots(env.home()));
    let written = if args.json {
        serde_json::to_writer_pretty(&mut *out, &report).map_err(io::Error::from).and_then(|()| writeln!(out))
    } else {
        let now = humantime::format_rfc3339_seconds(env.now()).to_string();
        out.write_all(text(&report, &loaded, env.home(), &now).as_bytes())
    };
    match written {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(ExitCode::SUCCESS),
        other => other.map(|()| ExitCode::SUCCESS).map_err(Into::into),
    }
}

/// The whole text: the summary, then the rows.
pub fn text(r: &ChangesReport, loaded: &Loaded, home: &Path, now: &str) -> String {
    let Some(reason) = r.reason else {
        let mut s = summary(r, loaded, home, now, usize::MAX);
        s.push_str(&rows(&r.items, home));
        return s;
    };
    format!("{}\n", not_compared(reason))
}

/// `ddc scan`'s block, printed after its report when its save kept the
/// report it replaced: `current` is the report just saved. A file that
/// cannot be read is a warning, not an error: the scan itself went well.
pub fn after_scan(out: &mut dyn Write, env: &Env, current: ScanReport) -> io::Result<()> {
    let loaded = match changes::load_previous(env, current) {
        Ok(loaded) => loaded,
        Err(e) => {
            eprintln!("ddc: warning: {e}");
            return Ok(());
        }
    };
    let journal = match changes::read_journal(env) {
        Ok(journal) => journal,
        Err(e) => {
            eprintln!("ddc: warning: cannot read {}: {e}", env.log_dir().join("actions.jsonl").display());
            return Ok(());
        }
    };
    let report = changes::compare_loaded(&loaded, &journal, &cloud_roots(env.home()));
    let now = humantime::format_rfc3339_seconds(env.now()).to_string();
    out.write_all(block(&report, &loaded, env.home(), &now).as_bytes())
}

/// The short form: the disk sentence, the groups that grew most, what lies
/// elsewhere and outside, and the person's cleans; or why the scans do not
/// compare, in one sentence.
pub fn block(r: &ChangesReport, loaded: &Loaded, home: &Path, now: &str) -> String {
    match r.reason {
        Some(reason) => format!("\n{}\n", not_compared(reason)),
        None => format!("\n{}`ddc changes` lists every row.\n", summary(r, loaded, home, now, BLOCK_GROUPS)),
    }
}

/// One sentence for a pair that was not compared.
pub fn not_compared(reason: Reason) -> &'static str {
    match reason {
        Reason::NoReport | Reason::NoPrevious | Reason::OtherSchema | Reason::PredatesFields => {
            "Growth shows after the next scan."
        }
        Reason::Cancelled => "The last scan or the one before it was stopped: growth shows after the next whole scan.",
        Reason::NoSharedRoot => {
            "The last two scans read different folders: growth shows after the next scan of the same ones."
        }
        Reason::OutOfStep => {
            "The two saved scans do not belong together: growth shows after the next scan of your usual folders."
        }
    }
}

/// The disk sentence, one line per group that grew (the `groups` biggest),
/// what lies elsewhere and outside, and the person's cleans.
pub fn summary(r: &ChangesReport, loaded: &Loaded, home: &Path, now: &str, groups: usize) -> String {
    let mut s = String::new();
    let since = r
        .baseline
        .as_ref()
        .and_then(|b| b.written_at.as_deref())
        .and_then(|at| age(at, now))
        .map(|a| format!(", {a}"))
        .unwrap_or_default();
    let mut lines: Vec<(String, i64, String)> =
        r.groups.iter().filter(|g| g.delta > 0).take(groups).map(|g| (g.group.clone(), g.delta, details(g))).collect();
    if let Some(home_) = &r.home {
        match home_.elsewhere {
            Some(d) if d != 0 => lines.push(("Elsewhere in your home folder".into(), d, String::new())),
            _ => {}
        }
    }
    if let Some(rest) = &r.rest
        && let Some(d) = rest.delta.filter(|d| *d != 0)
    {
        lines.push(("Not read, or outside your home folder".into(), d, kinds(&rest.not_read)));
    }
    match &r.disk {
        Some(d) if d.delta > 0 => s.push_str(&format!(
            "{} more in use on {} than at the earlier scan{since}.\n",
            human(d.delta.unsigned_abs()),
            mid_sentence(&volume_name(&d.mount_point))
        )),
        Some(d) if d.delta < 0 => s.push_str(&format!(
            "{} less in use on {} than at the earlier scan{since}.\n",
            human(d.delta.unsigned_abs()),
            mid_sentence(&volume_name(&d.mount_point))
        )),
        _ if lines.is_empty() => s.push_str(&format!("Nothing grew since the earlier scan{since}.\n")),
        _ => s.push_str(&format!("Since the earlier scan{since}:\n")),
    }
    if let Some(note) = scope_note(r, loaded, home) {
        s.push_str(&format!("{note}\n"));
    }
    let width = lines.iter().map(|(l, ..)| l.chars().count()).max().unwrap_or(0);
    for (label, delta, details) in &lines {
        let details = if details.is_empty() { String::new() } else { format!("  {details}") };
        s.push_str(&format!("  {label:<width$}  {:>10}{details}\n", signed(*delta)));
    }
    if let Some(h) = &r.home
        && !h.compared
    {
        s.push_str(&format!("The home folder as a whole does not compare: {}.\n", home_reasons(&h.reasons)));
    }
    if let Some(c) = r.cleaned_since.filter(|c| c.deleted + c.trashed > 0) {
        let total = human(c.deleted + c.trashed);
        if c.trashed > 0 {
            s.push_str(&format!("You cleaned {total} since then; {} of it is in the Trash.\n", human(c.trashed)));
        } else {
            s.push_str(&format!("You cleaned {total} since then.\n"));
        }
    }
    s
}

/// "Compared in ~/Projects only: …" when one scan read less than the other.
fn scope_note(r: &ChangesReport, loaded: &Loaded, home: &Path) -> Option<String> {
    let (b, c) = (loaded.baseline.as_ref()?, loaded.current.as_ref()?);
    let same = |roots: &[PathBuf]| {
        let (mut a, mut b) = (roots.to_vec(), r.compared_roots.clone());
        a.sort();
        b.sort();
        a == b
    };
    if same(&b.scan_roots) && same(&c.scan_roots) {
        return None;
    }
    let roots: Vec<String> = r.compared_roots.iter().map(|p| tilde(p, home)).collect();
    Some(format!("Compared in {} only: the other scan covered other folders.", roots.join(", ")))
}

/// "14 new, 2 grew, 9 in Claude Code worktrees".
fn details(g: &Group) -> String {
    let mut parts: Vec<String> =
        [(g.new, "new"), (g.grew, "grew"), (g.moved, "moved"), (g.shrank, "shrank"), (g.gone, "gone")]
            .iter()
            .filter(|(n, _)| *n > 0)
            .map(|(n, w)| format!("{n} {w}"))
            .collect();
    for h in g.by_harness.iter().filter(|h| h.delta > 0) {
        parts.push(format!("{} in {}", h.count, harness_place(h.harness)));
    }
    parts.join(", ")
}

fn harness_place(h: OriginHarness) -> &'static str {
    match h {
        OriginHarness::ClaudeCode => "Claude Code worktrees or data",
        OriginHarness::Codex => "Codex worktrees or data",
        OriginHarness::Opencode => "OpenCode worktrees or data",
        OriginHarness::Cursor => "Cursor worktrees or data",
        OriginHarness::Git => "git worktrees",
    }
}

/// The kinds of what was not read, counted: "2 cloud, 1 refused".
fn kinds(not_read: &[changes::NotRead]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for n in not_read {
        *counts.entry(kind_word(n.kind)).or_default() += 1;
    }
    counts.iter().map(|(w, n)| format!("{n} {w}")).collect::<Vec<_>>().join(", ")
}

fn kind_word(k: NotReadKind) -> &'static str {
    match k {
        NotReadKind::Cloud => "cloud",
        NotReadKind::Skip => "skipped by you",
        NotReadKind::Prompting => "behind a macOS prompt",
        NotReadKind::Refused => "refused",
        NotReadKind::Evicted => "cloud-only",
        NotReadKind::OtherDisk => "on other disks",
        NotReadKind::Snapshot => "snapshots",
        NotReadKind::Changed => "changed while read",
    }
}

fn home_reasons(reasons: &[HomeReason]) -> String {
    let words: Vec<&str> = reasons
        .iter()
        .map(|r| match r {
            HomeReason::DifferentHome => "the scans are of different home folders",
            HomeReason::DifferentRoots => "the scans read different folders",
            HomeReason::DifferentAccess => "one scan had Full Disk Access and the other did not",
            HomeReason::CutList => "a list of skipped folders was cut short",
            HomeReason::DifferentNotRead => "the scans could not read the same folders",
        })
        .collect();
    words.join("; ")
}

/// The rows: a change of at least 100 MiB or one of the 20 biggest, the rest
/// folded into one line.
pub fn rows(items: &[Item], home: &Path) -> String {
    let shown: Vec<&Item> =
        items.iter().enumerate().filter(|(i, it)| *i < BIGGEST || size(it) >= SHOWN_MIN).map(|(_, it)| it).collect();
    if shown.is_empty() {
        return String::new();
    }
    let mut s = String::from("\n");
    for it in &shown {
        let approx = if it.approximate { "≈" } else { "" };
        let kind = match it.kind {
            ItemKind::New => "new".to_owned(),
            ItemKind::Grew => "grew".to_owned(),
            ItemKind::Shrank if it.cleaned > 0 => format!("shrank, {} cleaned by you", human(it.cleaned)),
            ItemKind::Shrank => "shrank".to_owned(),
            ItemKind::Gone => "gone".to_owned(),
            ItemKind::Cleaned => "cleaned by you".to_owned(),
            ItemKind::Moved => {
                format!("moved from {}", it.moved_from.as_deref().map(|p| tilde(p, home)).unwrap_or_default())
            }
            ItemKind::NotCompared => "not compared".to_owned(),
        };
        s.push_str(&format!(
            "  {:>11}  {}  ({kind})\n",
            format!("{approx}{}", signed(it.delta)),
            tilde(&it.path, home)
        ));
    }
    let folded = items.len() - shown.len();
    if folded > 0 {
        s.push_str(&format!("  {folded} smaller changes\n"));
    }
    s
}

fn size(it: &Item) -> u64 {
    it.delta.unsigned_abs().max(it.cleaned)
}

/// "+1.0 GiB", "-512.0 MiB".
fn signed(delta: i64) -> String {
    let sign = if delta < 0 { "-" } else { "+" };
    format!("{sign}{}", human(delta.unsigned_abs()))
}

#[cfg(test)]
mod tests {
    use ddc_core::changes::{CHANGES_SCHEMA_VERSION, Cleaned, Disk, Home, NotRead, Rest, Side, Status};

    use super::*;

    const G: u64 = 1 << 30;
    const M: u64 = 1 << 20;

    fn item(path: &str, kind: ItemKind, delta: i64) -> Item {
        Item {
            path: PathBuf::from(path),
            rule: "rust.target".into(),
            kind,
            before: None,
            now: None,
            delta,
            cleaned: 0,
            trashed_in: None,
            moved_from: None,
            harness: None,
            ignored: false,
            approximate: false,
        }
    }

    fn compared(items: Vec<Item>) -> ChangesReport {
        ChangesReport {
            schema_version: CHANGES_SCHEMA_VERSION,
            status: Status::Compared,
            reason: None,
            baseline: Some(Side {
                written_at: Some("2026-11-03T10:00:00Z".into()),
                written_by: None,
                generated_at: "2026-11-03T10:00:00Z".into(),
            }),
            current: None,
            compared_roots: vec![PathBuf::from("/h")],
            disk: Some(Disk {
                mount_point: "/System/Volumes/Data".into(),
                before_in_use: 100 * G,
                now_in_use: 298 * G,
                delta: 198 * G as i64,
                before_at: "2026-11-03T10:00:00Z".into(),
                now_at: "2026-11-04T12:00:00Z".into(),
            }),
            home: Some(Home {
                compared: true,
                reasons: Vec::new(),
                delta: Some(190 * G as i64),
                ignored_delta: 0,
                elsewhere: Some(4 * G as i64),
            }),
            rest: Some(Rest {
                delta: Some(8 * G as i64),
                not_read: vec![
                    NotRead { path: "/h/Library/CloudStorage/x".into(), kind: NotReadKind::Cloud },
                    NotRead { path: "/h/Library/CloudStorage/y".into(), kind: NotReadKind::Cloud },
                    NotRead { path: "/h/z".into(), kind: NotReadKind::Refused },
                ],
            }),
            cleaned_since: Some(Cleaned { deleted: 28 * G, trashed: 12 * G }),
            groups: vec![
                Group {
                    group: "Build outputs".into(),
                    delta: 186 * G as i64,
                    new: 14,
                    grew: 2,
                    shrank: 0,
                    gone: 0,
                    cleaned: 3,
                    moved: 0,
                    by_harness: vec![changes::HarnessDelta {
                        harness: OriginHarness::ClaudeCode,
                        delta: 150 * G as i64,
                        count: 9,
                    }],
                },
                Group {
                    group: "node_modules".into(),
                    delta: -(2 * G as i64),
                    new: 0,
                    grew: 0,
                    shrank: 1,
                    gone: 0,
                    cleaned: 0,
                    moved: 0,
                    by_harness: Vec::new(),
                },
            ],
            items,
            grew: true,
        }
    }

    fn loaded() -> Loaded {
        Loaded { baseline: None, current: None, reasons: Vec::new() }
    }

    #[test]
    fn the_summary_says_the_disk_the_groups_that_grew_the_rest_and_your_cleans() {
        let text = summary(&compared(Vec::new()), &loaded(), Path::new("/h"), "2026-11-04T12:00:00Z", usize::MAX);
        assert_eq!(
            text,
            "198.0 GiB more in use on the startup disk than at the earlier scan, 26 h ago.\n\
             \x20 Build outputs                          +186.0 GiB  14 new, 2 grew, 9 in Claude Code worktrees or data\n\
             \x20 Elsewhere in your home folder            +4.0 GiB\n\
             \x20 Not read, or outside your home folder    +8.0 GiB  2 cloud, 1 refused\n\
             You cleaned 40.0 GiB since then; 12.0 GiB of it is in the Trash.\n"
        );
    }

    #[test]
    fn rows_under_100_mib_beyond_the_20_biggest_fold_into_one_line() {
        let mut items: Vec<Item> =
            (0..25).map(|i| item(&format!("/h/p{i:02}/target"), ItemKind::New, M as i64)).collect();
        items.insert(0, item("/h/big/target", ItemKind::Grew, 3 * G as i64));
        items.push(item("/h/late/target", ItemKind::Gone, -(200 * M as i64)));
        let mut moved = item("/h/b", ItemKind::Moved, 0);
        moved.moved_from = Some("/h/a".into());
        moved.approximate = true;
        items.push(moved);
        let text = rows(&items, Path::new("/h"));
        assert!(text.contains("   +3.0 GiB  ~/big/target  (grew)\n"), "{text}");
        assert!(text.contains(" -200.0 MiB  ~/late/target  (gone)\n"), "{text}");
        assert_eq!(text.lines().filter(|l| l.ends_with("(new)")).count(), 19, "{text}");
        assert!(text.ends_with("  7 smaller changes\n"), "{text}");
        let mut shared = item("/h/uv", ItemKind::Grew, 2 * G as i64);
        shared.approximate = true;
        assert_eq!(rows(&[shared], Path::new("/h")), "\n    ≈+2.0 GiB  ~/uv  (grew)\n");
        let small = rows(&[item("/h/b", ItemKind::Cleaned, -5)], Path::new("/h"));
        assert_eq!(small, "\n         -5 B  ~/b  (cleaned by you)\n");
    }

    #[test]
    fn a_project_scan_against_a_home_scan_says_where_it_compared() {
        let mut r = compared(Vec::new());
        r.compared_roots = vec![PathBuf::from("/h/Projects")];
        let report = |roots: &[&str]| -> ddc_core::model::ScanReport {
            let mut s: ddc_core::model::ScanReport = serde_json::from_str(
                r#"{"schema_version":8,"ddc_version":"0","platform":"macos","capabilities":[],"generated_at":"x",
                   "duration_ms":0,"home":"/h","scan_roots":[],"full_disk_access":true,"min_age_days":14,
                   "totals":{"scanned_bytes":0,"files":0,"dirs":0,"unowned_bytes":0,"classes":[],"selected_bytes":0,
                     "selected_reclaimable_bytes":0},
                   "findings":[],"large_files":[],"inaccessible":[],"errors_total":0,"skipped":[],"skipped_total":0,
                   "cancelled":false}"#,
            )
            .unwrap();
            s.scan_roots = roots.iter().map(PathBuf::from).collect();
            s
        };
        let pair =
            Loaded { baseline: Some(report(&["/h"])), current: Some(report(&["/h/Projects"])), reasons: Vec::new() };
        let text = summary(&r, &pair, Path::new("/h"), "2026-11-04T12:00:00Z", usize::MAX);
        assert!(text.contains("\nCompared in ~/Projects only: the other scan covered other folders.\n"), "{text}");
        let same = Loaded { baseline: Some(report(&["/h"])), current: Some(report(&["/h"])), reasons: Vec::new() };
        r.compared_roots = vec![PathBuf::from("/h")];
        assert!(!summary(&r, &same, Path::new("/h"), "x", usize::MAX).contains("Compared in"));
    }

    #[test]
    fn a_pair_not_compared_says_why_in_one_sentence() {
        for (reason, start) in [
            (Reason::NoReport, "Growth shows after the next scan."),
            (Reason::PredatesFields, "Growth shows after the next scan."),
            (Reason::Cancelled, "The last scan or the one before it was stopped"),
            (Reason::NoSharedRoot, "The last two scans read different folders"),
            (Reason::OutOfStep, "The two saved scans do not belong together"),
        ] {
            let r = ChangesReport::not_compared(reason, None, None);
            let t = text(&r, &loaded(), Path::new("/h"), "2026-11-04T12:00:00Z");
            assert!(t.starts_with(start) && t.ends_with(".\n") && t.lines().count() == 1, "{reason:?}: {t}");
        }
    }

    #[test]
    fn scans_block_names_the_three_groups_that_grew_most_and_points_at_ddc_changes() {
        let mut r = compared(Vec::new());
        let group = |name: &str, gib: i64| Group {
            group: name.into(),
            delta: gib * G as i64,
            new: 1,
            grew: 0,
            shrank: 0,
            gone: 0,
            cleaned: 0,
            moved: 0,
            by_harness: Vec::new(),
        };
        r.groups = vec![group("Build outputs", 9), group("Caches", 5), group("node_modules", 3), group("Logs", 1)];
        let text = block(&r, &loaded(), Path::new("/h"), "2026-11-04T12:00:00Z");
        assert!(text.starts_with("\n198.0 GiB more in use on the startup disk"), "{text}");
        assert!(text.contains("  node_modules ") && !text.contains("Logs"), "{text}");
        assert!(text.contains("Elsewhere in your home folder") && text.contains("Not read, or outside"), "{text}");
        assert!(text.ends_with("of it is in the Trash.\n`ddc changes` lists every row.\n"), "{text}");
        assert!(!text.contains("(new)"), "the block has no rows: {text}");
        let mut still = compared(Vec::new());
        (still.disk, still.home, still.rest, still.cleaned_since) = (None, None, None, None);
        still.groups.retain(|g| g.delta <= 0);
        assert_eq!(
            block(&still, &loaded(), Path::new("/h"), "2026-11-04T12:00:00Z"),
            "\nNothing grew since the earlier scan, 26 h ago.\n`ddc changes` lists every row.\n"
        );
        let not = ChangesReport::not_compared(Reason::PredatesFields, None, None);
        assert_eq!(block(&not, &loaded(), Path::new("/h"), "x"), "\nGrowth shows after the next scan.\n");
    }

    #[test]
    fn a_home_that_does_not_compare_says_why_and_a_shrinking_disk_says_less() {
        let mut r = compared(Vec::new());
        r.disk.as_mut().unwrap().delta = -(3 * G as i64);
        r.home = Some(Home {
            compared: false,
            reasons: vec![HomeReason::DifferentAccess, HomeReason::CutList],
            delta: None,
            ignored_delta: 0,
            elsewhere: None,
        });
        r.rest = None;
        r.cleaned_since = Some(Cleaned { deleted: G, trashed: 0 });
        let text = summary(&r, &loaded(), Path::new("/h"), "not a time", usize::MAX);
        assert!(text.starts_with("3.0 GiB less in use on the startup disk than at the earlier scan.\n"), "{text}");
        assert!(text.contains(
            "The home folder as a whole does not compare: one scan had Full Disk Access and the other did not; \
             a list of skipped folders was cut short.\n"
        ));
        assert!(text.ends_with("You cleaned 1.0 GiB since then.\n"), "{text}");
        assert!(!text.contains("Elsewhere"), "{text}");
    }
}
