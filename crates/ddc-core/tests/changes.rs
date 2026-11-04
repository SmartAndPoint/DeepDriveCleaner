//! `ddc changes` (story "ddc changes: what grew between the last two
//! scans", DDC-001): the comparison on hand-built reports and journal
//! lines. Nothing here scans, so every test runs on every platform.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use ddc_core::changes::{
    self, ChangesReport, HomeReason, Item, ItemKind, NotReadKind, Reason, Status, compare, compare_loaded,
    parse_journal,
};
use ddc_core::env::Env;
use ddc_core::model::{Finding, OriginHarness, ScanReport, SkippedPath};
use ddc_core::scan::SkipReason;

const G: u64 = 1 << 30;
const M: u64 = 1 << 20;
const BEFORE: &str = "2026-11-03T10:00:00Z";
const BETWEEN: &str = "2026-11-03T12:00:00Z";
const NOW: &str = "2026-11-04T10:00:00Z";
/// The rules both reports had, unless a test says otherwise.
const RULES: &[&str] = &[
    "cache.homebrew",
    "claude.sessions",
    "git.worktree",
    "node.node_modules",
    "python.pycache",
    "rust.target",
    "trash",
    "user.downloads",
];

/// A report of `/h` written at `written_at`, with a volume of `free` bytes
/// free out of 1000 GiB.
fn report(written_at: &str, free: u64) -> ScanReport {
    let mut r: ScanReport = serde_json::from_str(&format!(
        r#"{{"schema_version":{v},"ddc_version":"0","platform":"macos","capabilities":[],"generated_at":"{written_at}",
           "duration_ms":0,"home":"/h","scan_roots":["/h"],"full_disk_access":true,"min_age_days":14,
           "volumes":[{{"mount_point":"/","roots":["/h"],"free_bytes":{free},"total_bytes":{total}}}],
           "totals":{{"scanned_bytes":0,"files":0,"dirs":0,"unowned_bytes":0,"classes":[],"selected_bytes":0,
             "selected_reclaimable_bytes":0}},
           "findings":[],"large_files":[],"inaccessible":[],"errors_total":0,"skipped":[],"skipped_total":0,
           "cancelled":false,"written_at":"{written_at}"}}"#,
        v = ddc_core::model::SCHEMA_VERSION,
        total = 1000 * G,
    ))
    .unwrap();
    r.rules_in_force = Some(RULES.iter().map(|s| (*s).to_owned()).collect());
    r
}

/// A finding at `path` of `rule`, `bytes` big, with inode `ino`.
fn finding(path: &str, rule: &str, bytes: u64, ino: u64) -> Finding {
    serde_json::from_str(&format!(
        r#"{{"path":"{path}","rule":"{rule}","category":"project-artifact","class":"rebuildable",
           "rule_class":"rebuildable","removal":"delete","bytes":{bytes},"shared_bytes":0,"files":1,"dirs":1,
           "incomplete":false,"selected":false,"notes":[],"hint":"","dev":1,"ino":{ino}}}"#
    ))
    .unwrap()
}

/// `findings` in `r`, with `scanned_bytes` their sum plus `unowned`.
fn with(mut r: ScanReport, findings: Vec<Finding>, unowned: u64) -> ScanReport {
    r.totals.scanned_bytes = findings.iter().map(|f| f.bytes).sum::<u64>() + unowned;
    r.totals.unowned_bytes = unowned;
    r.findings = findings;
    r
}

/// The pair: `before`, and `now` naming it as its previous report.
fn pair(before: ScanReport, mut now: ScanReport) -> (ScanReport, ScanReport) {
    now.previous_written_at = before.written_at.clone();
    (before, now)
}

fn changes(before: &ScanReport, now: &ScanReport, journal: &str) -> ChangesReport {
    compare(before, now, &parse_journal(journal), &[PathBuf::from("/h/Library/CloudStorage")])
}

fn item<'a>(r: &'a ChangesReport, path: &str) -> &'a Item {
    r.items.iter().find(|i| i.path == Path::new(path)).unwrap_or_else(|| panic!("no row at {path}: {:#?}", r.items))
}

fn has(r: &ChangesReport, path: &str) -> bool {
    r.items.iter().any(|i| i.path == Path::new(path))
}

fn group<'a>(r: &'a ChangesReport, label: &str) -> &'a changes::Group {
    r.groups.iter().find(|g| g.group == label).unwrap_or_else(|| panic!("no group {label}: {:#?}", r.groups))
}

/// An item line of the journal.
fn line(ts: &str, path: &str, ino: u64, bytes: u64, outcome: &str) -> String {
    format!(
        r#"{{"schema_version":11,"kind":"item","ts":"{ts}","path":"{path}","dev":1,"ino":{ino},"rule":"r","class":"safe","method":"delete","bytes":{bytes},"outcome":"{outcome}"}}"#
    )
}

fn skipped(path: &str, reason: SkipReason) -> SkippedPath {
    SkippedPath { path: PathBuf::from(path), reason }
}

#[test]
fn a_finding_where_one_side_did_not_read_is_not_compared() {
    let mut before = report(BEFORE, 500 * G);
    // A scheduled scan without Full Disk Access left Downloads out.
    before.skipped = vec![
        skipped("/h/Downloads", SkipReason::Excluded),
        skipped("/h/vol", SkipReason::HoldsSubvolume),
        skipped("/h/Library/CloudStorage/x", SkipReason::Excluded),
    ];
    before.skipped_total = 3;
    before.not_scanned = vec![PathBuf::from("/h/skipped")];
    let before = with(before, vec![finding("/h/Pictures/old/node_modules", "node.node_modules", G, 1)], 0);
    let mut now = report(NOW, 500 * G);
    now.skipped = vec![skipped("/h/Pictures", SkipReason::Excluded)];
    now.skipped_total = 1;
    let now = with(
        now,
        vec![
            finding("/h/Downloads/p/node_modules", "node.node_modules", G, 2),
            finding("/h/skipped/node_modules", "node.node_modules", G, 3),
            finding("/h/vol/target", "rust.target", G, 4),
        ],
        0,
    );
    let (before, now) = pair(before, now);
    let r = changes(&before, &now, "");

    assert_eq!(item(&r, "/h/Downloads/p/node_modules").kind, ItemKind::NotCompared, "read now, not before");
    assert_eq!(item(&r, "/h/Pictures/old/node_modules").kind, ItemKind::NotCompared, "read before, not now");
    assert_eq!(item(&r, "/h/skipped/node_modules").kind, ItemKind::NotCompared, "a skip removed since");
    assert_eq!(item(&r, "/h/vol/target").kind, ItemKind::New, "holds-subvolume was read");
    assert!(r.groups.iter().all(|g| g.group != "node_modules"), "{:#?}", r.groups);
    let kinds = |rep: &ScanReport| changes::not_read(rep, &[PathBuf::from("/h/Library/CloudStorage")]);
    let before_kinds: Vec<(PathBuf, NotReadKind)> = kinds(&before).into_iter().map(|n| (n.path, n.kind)).collect();
    assert_eq!(
        before_kinds,
        [
            (PathBuf::from("/h/skipped"), NotReadKind::Skip),
            (PathBuf::from("/h/Downloads"), NotReadKind::Prompting),
            (PathBuf::from("/h/Library/CloudStorage/x"), NotReadKind::Cloud),
        ]
    );
}

#[test]
fn every_skip_reason_has_its_kind_and_an_inaccessible_folder_is_refused() {
    let mut r = report(NOW, 500 * G);
    r.not_scanned = vec![PathBuf::from("/h/mine")];
    r.skipped = vec![
        skipped("/h/mine/deep", SkipReason::Changed),
        skipped("/h/a", SkipReason::Dataless),
        skipped("/h/b", SkipReason::OtherFilesystem),
        skipped("/h/c", SkipReason::Automount),
        skipped("/h/d", SkipReason::Snapshot),
        skipped("/h/e", SkipReason::Changed),
    ];
    r.inaccessible = vec![ddc_core::model::Inaccessible { path: PathBuf::from("/h/f"), error: "EPERM".into() }];
    let kinds: Vec<NotReadKind> = changes::not_read(&r, &[]).into_iter().map(|n| n.kind).collect();
    use NotReadKind::*;
    assert_eq!(kinds, [Skip, Skip, Evicted, OtherDisk, OtherDisk, Snapshot, Changed, Refused]);
}

#[test]
fn a_rule_the_baseline_lacked_is_not_compared_and_a_type_switched_on_compares() {
    let mut before = with(report(BEFORE, 500 * G), vec![finding("/h/p/node_modules", "node.node_modules", G, 1)], 0);
    before.findings[0].ignored = true;
    before.rules_in_force.as_mut().unwrap().retain(|r| r != "rust.target");
    let now = with(
        report(NOW, 500 * G),
        vec![finding("/h/p/node_modules", "node.node_modules", 2 * G, 1), finding("/h/p/target", "rust.target", G, 2)],
        0,
    );
    let (before, now) = pair(before, now);
    let r = changes(&before, &now, "");
    assert_eq!(item(&r, "/h/p/target").kind, ItemKind::NotCompared, "a rule new in this version");
    let modules = item(&r, "/h/p/node_modules");
    assert_eq!((modules.kind, modules.delta, modules.ignored), (ItemKind::Grew, G as i64, false));
    assert_eq!(group(&r, "node_modules").delta, G as i64);
}

#[test]
fn a_target_rebuilt_in_place_is_one_row_and_a_renamed_worktree_folder_moved() {
    let before = with(
        report(BEFORE, 500 * G),
        vec![
            finding("/h/p/target", "rust.target", 3 * G, 1),
            finding("/h/p/.claude/worktrees/a", "git.worktree", G, 9),
        ],
        0,
    );
    let now = with(
        report(NOW, 500 * G),
        vec![
            finding("/h/p/target", "rust.target", 5 * G, 7),
            finding("/h/p/.claude/worktrees/b", "git.worktree", G + M, 9),
        ],
        0,
    );
    let (before, now) = pair(before, now);
    let r = changes(&before, &now, "");
    let target = item(&r, "/h/p/target");
    assert_eq!((target.kind, target.delta), (ItemKind::Grew, 2 * G as i64));
    let moved = item(&r, "/h/p/.claude/worktrees/b");
    assert_eq!((moved.kind, moved.delta), (ItemKind::Moved, M as i64));
    assert_eq!(moved.moved_from.as_deref(), Some(Path::new("/h/p/.claude/worktrees/a")));
    assert!(!has(&r, "/h/p/.claude/worktrees/a"), "no gone row for it: {:#?}", r.items);
    assert_eq!(r.items.len(), 2);
}

#[test]
fn the_persons_cleans_are_cleaned_not_gone_and_the_trash_takes_what_they_trashed() {
    let before = with(
        report(BEFORE, 500 * G),
        vec![
            finding("/h/a/target", "rust.target", 2 * G, 1),
            finding("/h/b/target", "rust.target", G, 2),
            finding("/h/c/target", "rust.target", G, 3),
            finding("/h/d/__pycache__", "python.pycache", M, 4),
            finding("/h/Library/Caches/Homebrew", "cache.homebrew", 3 * G, 5),
            finding("/h/.Trash", "trash", G, 6),
        ],
        0,
    );
    let now = with(
        report(NOW, 500 * G),
        vec![
            finding("/h/d/__pycache__", "python.pycache", M, 4),
            finding("/h/Library/Caches/Homebrew", "cache.homebrew", G, 5),
            finding("/h/.Trash", "trash", G + 512 * M, 6),
        ],
        0,
    );
    let (before, now) = pair(before, now);
    let journal = [
        line(BETWEEN, "/h/a/target", 1, 2 * G, "deleted"),
        line(BETWEEN, "/h/b/target", 2, G, "trashed"),
        // The same path with another inode explains nothing.
        line(BETWEEN, "/h/c/target", 99, G, "deleted"),
        // Before the baseline was saved: not between the two.
        line("2026-11-02T10:00:00Z", "/h/d/__pycache__", 4, M, "deleted"),
        format!(
            r#"{{"schema_version":11,"kind":"area-action","ts":"{BETWEEN}","action":"brew-cleanup","area":"homebrew","command":"brew cleanup","result":{{"status":"freed"}}}}"#
        ),
        "not json".to_owned(),
    ]
    .join("\n");
    let r = changes(&before, &now, &journal);

    let a = item(&r, "/h/a/target");
    assert_eq!((a.kind, a.cleaned), (ItemKind::Cleaned, 2 * G));
    assert_eq!(item(&r, "/h/b/target").kind, ItemKind::Cleaned);
    assert_eq!(item(&r, "/h/c/target").kind, ItemKind::Gone, "removed in Finder");
    let brew = item(&r, "/h/Library/Caches/Homebrew");
    assert_eq!((brew.kind, brew.cleaned), (ItemKind::Cleaned, 2 * G), "the area line without sizes takes the shrink");
    let trash = item(&r, "/h/.Trash");
    assert_eq!(trash.trashed_in, Some(512 * M), "its growth, at most what was trashed");
    let since = r.cleaned_since.unwrap();
    assert_eq!((since.deleted, since.trashed), (4 * G, G), "a's 2 GiB and brew's 2 GiB; b trashed");
    assert!(!has(&r, "/h/d/__pycache__"));
    assert_eq!(group(&r, "Build outputs").delta, -(G as i64), "only c, which no clean of yours removed");
    assert_eq!(group(&r, "Build outputs").cleaned, 2);
    assert_eq!(group(&r, "Trash").delta, 0);
}

#[test]
fn an_area_line_with_sizes_credits_what_it_freed_and_the_rest_is_a_shrink() {
    let before =
        with(report(BEFORE, 500 * G), vec![finding("/h/Library/Caches/Homebrew", "cache.homebrew", 4 * G, 5)], 0);
    let now = with(report(NOW, 500 * G), vec![finding("/h/Library/Caches/Homebrew", "cache.homebrew", G, 5)], 0);
    let (before, now) = pair(before, now);
    let journal = format!(
        r#"{{"kind":"area-action","ts":"{BETWEEN}","action":"brew-cleanup","bytes_before":{b},"bytes_after":{a},"result":{{"status":"freed"}}}}
{{"kind":"area-action","ts":"{BETWEEN}","action":"brew-cleanup","bytes_before":{b},"bytes_after":0,"result":{{"status":"refused","reason":"x"}}}}"#,
        b = 4 * G,
        a = 2 * G,
    );
    let r = changes(&before, &now, &journal);
    let brew = item(&r, "/h/Library/Caches/Homebrew");
    assert_eq!((brew.kind, brew.cleaned, brew.delta), (ItemKind::Shrank, 2 * G, -(3 * G as i64)));
    assert_eq!(group(&r, "Package downloads").delta, -(G as i64));
}

#[test]
fn a_picked_entry_and_a_sessions_companions_count_against_their_folder() {
    let mut downloads = finding("/h/Downloads", "user.downloads", 10 * G, 1);
    downloads.contents = Some(serde_json::from_str(r#"{"entries":[],"rest_count":0,"rest_bytes":0}"#).unwrap());
    let mut sessions = finding("/h/.claude/projects/p", "claude.sessions", 3 * G, 2);
    sessions.contents = downloads.contents.clone();
    let before = with(report(BEFORE, 500 * G), vec![downloads.clone(), sessions.clone()], 0);
    downloads.bytes = 6 * G;
    sessions.bytes = G;
    let now = with(report(NOW, 500 * G), vec![downloads, sessions], 0);
    let (before, now) = pair(before, now);
    let companion = format!(
        r#"{{"kind":"item","ts":"{BETWEEN}","path":"/h/.claude/todos/s1.json","dev":1,"ino":77,"bytes":{b},"entry":"/h/.claude/projects/p/s1.jsonl","outcome":"trashed"}}"#,
        b = G
    );
    let journal = [
        line(BETWEEN, "/h/Downloads/big.dmg", 50, 3 * G, "trashed"),
        line(BETWEEN, "/h/.claude/projects/p/s1.jsonl", 51, G, "trashed"),
        companion,
    ]
    .join("\n");
    let r = changes(&before, &now, &journal);
    let d = item(&r, "/h/Downloads");
    assert_eq!((d.kind, d.cleaned, d.delta), (ItemKind::Shrank, 3 * G, -(4 * G as i64)), "up to the line's bytes");
    let s = item(&r, "/h/.claude/projects/p");
    assert_eq!((s.kind, s.cleaned), (ItemKind::Cleaned, 2 * G), "the transcript and its companion");
    assert_eq!(r.cleaned_since.unwrap().trashed, 5 * G);
    assert!(r.items.iter().all(|i| !i.path.ends_with("big.dmg") && !i.path.ends_with("s1.jsonl")));
    assert_eq!(r.items.len(), 2);
}

#[test]
fn the_home_compares_only_when_both_scans_read_it_the_same_way() {
    let before = with(report(BEFORE, 500 * G), vec![finding("/h/p/target", "rust.target", G, 1)], 0);
    let mut now = with(report(NOW, 500 * G), vec![finding("/h/p/target", "rust.target", 2 * G, 1)], 0);
    now.full_disk_access = false;
    let (b, n) = pair(before.clone(), now);
    let r = changes(&b, &n, "");
    let home = r.home.as_ref().unwrap();
    assert_eq!((home.compared, home.delta, home.elsewhere), (false, None, None));
    assert_eq!(home.reasons, [HomeReason::DifferentAccess]);
    assert_eq!(group(&r, "Build outputs").delta, G as i64, "the groups still compare");

    // Folders that changed while the walk listed them do not stop it.
    let mut now = with(report(NOW, 500 * G), vec![finding("/h/p/target", "rust.target", 2 * G, 1)], 0);
    now.skipped = vec![skipped("/h/q/target", SkipReason::Changed)];
    now.skipped_total = 1;
    let (b, n) = pair(before.clone(), now);
    assert!(changes(&b, &n, "").home.unwrap().compared);

    // A cut list does.
    let mut now = with(report(NOW, 500 * G), vec![finding("/h/p/target", "rust.target", 2 * G, 1)], 0);
    now.skipped = vec![skipped("/h/q/target", SkipReason::Changed)];
    now.skipped_total = 2;
    let (b, n) = pair(before, now);
    assert_eq!(changes(&b, &n, "").home.unwrap().reasons, [HomeReason::CutList]);
}

/// The sums, written by hand.
#[test]
fn the_sums_split_the_disks_growth_into_groups_cleans_the_trash_elsewhere_and_the_rest() {
    let mut ignored = finding("/h/models/m", "node.node_modules", G, 20);
    ignored.ignored = true;
    let before = with(
        report(BEFORE, 500 * G),
        vec![
            finding("/h/p/target", "rust.target", 5 * G, 1),
            finding("/h/x/target", "rust.target", 3 * G, 2),
            finding("/h/y/target", "rust.target", G, 3),
            finding("/h/.Trash", "trash", 2 * G, 4),
            ignored.clone(),
        ],
        6 * G,
    );
    ignored.bytes = G + 512 * M;
    let now = with(
        report(NOW, 480 * G),
        vec![
            finding("/h/p/target", "rust.target", 7 * G, 1),
            finding("/h/new/target", "rust.target", 10 * G, 5),
            finding("/h/.Trash", "trash", 3 * G + 512 * M, 4),
            ignored,
        ],
        10 * G,
    );
    let (before, now) = pair(before, now);
    let journal =
        [line(BETWEEN, "/h/x/target", 2, 3 * G, "deleted"), line(BETWEEN, "/h/y/target", 3, G, "trashed")].join("\n");
    let r = changes(&before, &now, &journal);

    assert_eq!(r.status, Status::Compared);
    let build = group(&r, "Build outputs");
    assert_eq!((build.delta, build.new, build.grew, build.cleaned), (12 * G as i64, 1, 1, 2));
    let since = r.cleaned_since.unwrap();
    assert_eq!((since.deleted, since.trashed), (3 * G, G));
    assert_eq!(item(&r, "/h/.Trash").trashed_in, Some(G));
    assert_eq!(group(&r, "Trash").delta, 512 * M as i64);
    let home = r.home.as_ref().unwrap();
    assert_eq!(home.ignored_delta, 512 * M as i64);
    assert_eq!(home.delta, Some(14 * G as i64));
    assert_eq!(home.elsewhere, Some(4 * G as i64));
    assert_eq!(r.disk.as_ref().unwrap().delta, 20 * G as i64);
    assert_eq!(r.rest.as_ref().unwrap().delta, Some(6 * G as i64));
    assert!(r.groups.iter().all(|g| g.group != "node_modules"), "ignored findings stay out of the groups");
}

#[test]
fn an_artifact_in_an_agents_worktree_counts_for_that_agent() {
    let mut wt = finding("/h/p/.claude/worktrees/x", "git.worktree", M, 1);
    wt.origin = Some(serde_json::from_str(r#"{"harness":"claude-code","kind":"worktree"}"#).unwrap());
    let mut target = finding("/h/p/.claude/worktrees/x/target", "rust.target", 9 * G, 2);
    target.project = Some(serde_json::from_str(r#"{"root":"/h/p/.claude/worktrees/x","worktree_of":"/h/p"}"#).unwrap());
    let mut plain = finding("/h/q/target", "rust.target", G, 3);
    plain.project = Some(serde_json::from_str(r#"{"root":"/h/q"}"#).unwrap());
    let before = with(report(BEFORE, 500 * G), vec![wt.clone()], 0);
    let now = with(report(NOW, 500 * G), vec![wt, target, plain], 0);
    let (before, now) = pair(before, now);
    let r = changes(&before, &now, "");
    assert_eq!(item(&r, "/h/p/.claude/worktrees/x/target").harness, Some(OriginHarness::ClaudeCode));
    assert_eq!(item(&r, "/h/q/target").harness, None);
    let build = group(&r, "Build outputs");
    assert_eq!(build.by_harness.len(), 1);
    assert_eq!(
        (build.by_harness[0].harness, build.by_harness[0].delta, build.by_harness[0].count),
        (OriginHarness::ClaudeCode, 9 * G as i64, 1)
    );
}

#[test]
fn a_project_scan_against_a_home_scan_compares_under_the_project_only() {
    let before = with(
        report(BEFORE, 500 * G),
        vec![finding("/h/p/target", "rust.target", G, 1), finding("/h/q/target", "rust.target", G, 2)],
        0,
    );
    let mut now = with(report(NOW, 500 * G), vec![finding("/h/p/target", "rust.target", 3 * G, 1)], 0);
    now.scan_roots = vec![PathBuf::from("/h/p")];
    now.volumes[0].roots = vec![PathBuf::from("/h/p")];
    let (before, now) = pair(before, now);
    let r = changes(&before, &now, "");
    assert_eq!(r.compared_roots, [PathBuf::from("/h/p")]);
    assert_eq!(r.items.len(), 1, "q is outside: {:#?}", r.items);
    assert_eq!(r.home.as_ref().unwrap().reasons, [HomeReason::DifferentRoots]);
    assert_eq!(r.rest.as_ref().unwrap().delta, Some(0 - 2 * G as i64), "the disk's 0 minus the findings' 2 GiB");
}

/// A journaled clean removed the folder where it was; a new folder that
/// took its inode (ext4 reuses them quickly) is new, not a move.
#[test]
fn a_cleaned_folder_whose_inode_comes_back_elsewhere_is_cleaned_not_moved() {
    let before = with(report(BEFORE, 500 * G), vec![finding("/h/a/target", "rust.target", 2 * G, 1)], 0);
    let now = with(report(NOW, 500 * G), vec![finding("/h/b/target", "rust.target", 3 * G, 1)], 0);
    let (before, now) = pair(before, now);
    let r = changes(&before, &now, &line(BETWEEN, "/h/a/target", 1, 2 * G, "deleted"));
    assert_eq!(item(&r, "/h/a/target").kind, ItemKind::Cleaned);
    let b = item(&r, "/h/b/target");
    assert_eq!((b.kind, b.moved_from.as_deref()), (ItemKind::New, None));
    assert_eq!(r.cleaned_since.unwrap().deleted, 2 * G);
    assert_eq!(group(&r, "Build outputs").delta, 3 * G as i64);
}

/// A finding found on both sides is not compared either when the other
/// side did not read where it lies.
#[test]
fn a_finding_in_both_reports_under_a_folder_one_did_not_read_is_not_compared() {
    let before = with(report(BEFORE, 500 * G), vec![finding("/h/p/target", "rust.target", G, 1)], 0);
    let mut now = with(report(NOW, 500 * G), vec![finding("/h/p/target", "rust.target", 4 * G, 1)], 0);
    now.inaccessible =
        vec![ddc_core::model::Inaccessible { path: PathBuf::from("/h/p/target"), error: "EPERM".into() }];
    let (before, now) = pair(before, now);
    let r = changes(&before, &now, "");
    let t = item(&r, "/h/p/target");
    assert_eq!((t.kind, t.before, t.now), (ItemKind::NotCompared, Some(G), Some(4 * G)));
    assert!(r.groups.is_empty(), "{:#?}", r.groups);
}

/// A person's skip is in `not_scanned` and in `skipped`: listed once.
#[test]
fn a_folder_listed_twice_as_not_read_is_listed_once_with_its_first_kind() {
    let mut r = report(NOW, 500 * G);
    r.not_scanned = vec![PathBuf::from("/h/mine"), PathBuf::from("/h/Library/CloudStorage/x")];
    r.skipped =
        vec![skipped("/h/mine", SkipReason::Excluded), skipped("/h/Library/CloudStorage/x", SkipReason::Excluded)];
    r.inaccessible = vec![ddc_core::model::Inaccessible { path: PathBuf::from("/h/mine"), error: "EPERM".into() }];
    let listed: Vec<(PathBuf, NotReadKind)> = changes::not_read(&r, &[PathBuf::from("/h/Library/CloudStorage")])
        .into_iter()
        .map(|n| (n.path, n.kind))
        .collect();
    assert_eq!(
        listed,
        [
            (PathBuf::from("/h/mine"), NotReadKind::Skip),
            (PathBuf::from("/h/Library/CloudStorage/x"), NotReadKind::Cloud),
        ]
    );
}

#[test]
fn a_folder_that_went_with_no_journal_line_is_gone() {
    let before = with(report(BEFORE, 500 * G), vec![finding("/h/finder/target", "rust.target", G, 1)], 0);
    let now = with(report(NOW, 500 * G), Vec::new(), 0);
    let (before, now) = pair(before, now);
    let r = changes(&before, &now, &line(BETWEEN, "/h/other/target", 7, G, "deleted"));
    let t = item(&r, "/h/finder/target");
    assert_eq!((t.kind, t.cleaned, t.delta), (ItemKind::Gone, 0, -(G as i64)));
    assert_eq!(r.cleaned_since.unwrap(), changes::Cleaned::default(), "a line naming no finding counts nowhere");
}

struct Data {
    _tmp: tempfile::TempDir,
    env: Env,
}

impl Data {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let env = Env::with_paths(
            tmp.path(),
            &tmp.path().join("data"),
            &tmp.path().join("logs"),
            UNIX_EPOCH + Duration::from_secs(1),
        )
        .unwrap();
        fs::create_dir_all(env.data_dir()).unwrap();
        Self { _tmp: tmp, env }
    }

    fn put(&self, name: &str, r: &ScanReport) {
        fs::write(self.env.data_dir().join(name), serde_json::to_vec(r).unwrap()).unwrap();
    }

    fn put_bytes(&self, name: &str, bytes: &[u8]) {
        fs::write(self.env.data_dir().join(name), bytes).unwrap();
    }

    fn changes(&self) -> ChangesReport {
        let loaded = changes::load(&self.env).unwrap();
        compare_loaded(&loaded, &[], &[])
    }
}

/// The reason, after checking the shape of a pair not compared: the reports'
/// sides, and nulls and empty lists for the rest.
fn reason(r: &ChangesReport) -> Option<Reason> {
    assert_eq!(r.status == Status::Compared, r.reason.is_none());
    if r.reason.is_some() {
        let json = serde_json::to_value(r).unwrap();
        for key in ["disk", "home", "rest", "cleaned_since"] {
            assert!(json[key].is_null(), "{key}: {json}");
        }
        for key in ["compared_roots", "groups", "items"] {
            assert_eq!(json[key], serde_json::json!([]), "{key}: {json}");
        }
        assert_eq!(json["status"], "not-compared");
    }
    r.reason
}

#[test]
fn each_reason_not_to_compare_and_the_first_wins() {
    let d = Data::new();
    let base = with(report(BEFORE, 500 * G), vec![finding("/h/p/target", "rust.target", G, 1)], 0);
    let (b, n) =
        pair(base.clone(), with(report(NOW, 500 * G), vec![finding("/h/p/target", "rust.target", 2 * G, 1)], 0));
    assert_eq!(reason(&d.changes()), Some(Reason::NoReport));
    d.put("last-scan.json", &n);
    assert_eq!(reason(&d.changes()), Some(Reason::NoPrevious));
    d.put_bytes("previous-scan.json", br#"{"schema_version":1}"#);
    let r = d.changes();
    assert_eq!(reason(&r), Some(Reason::OtherSchema));
    let json = serde_json::to_value(&r).unwrap();
    assert!(json["baseline"].is_null() && json["disk"].is_null() && json["home"].is_null(), "{json}");
    assert!(json["rest"].is_null() && json["cleaned_since"].is_null(), "{json}");
    assert_eq!(
        (json["compared_roots"].clone(), json["groups"].clone(), json["items"].clone()),
        (serde_json::json!([]), serde_json::json!([]), serde_json::json!([]))
    );
    assert_eq!(json["current"]["written_at"], NOW);

    let mut old = b.clone();
    old.rules_in_force = None;
    d.put("previous-scan.json", &old);
    assert_eq!(reason(&d.changes()), Some(Reason::PredatesFields));
    // Either side: a current report that does not decode, or lacks S1's fields.
    d.put("previous-scan.json", &b);
    d.put_bytes("last-scan.json", b"{");
    assert_eq!(reason(&d.changes()), Some(Reason::OtherSchema));
    let mut unstamped = n.clone();
    unstamped.written_at = None;
    d.put("last-scan.json", &unstamped);
    assert_eq!(reason(&d.changes()), Some(Reason::PredatesFields));
    d.put("last-scan.json", &n);
    let mut cancelled = b.clone();
    cancelled.cancelled = true;
    d.put("previous-scan.json", &cancelled);
    assert_eq!(reason(&d.changes()), Some(Reason::Cancelled));
    let mut elsewhere = b.clone();
    elsewhere.scan_roots = vec![PathBuf::from("/other")];
    d.put("previous-scan.json", &elsewhere);
    assert_eq!(reason(&d.changes()), Some(Reason::NoSharedRoot));
    let mut stray = b.clone();
    stray.written_at = Some(BETWEEN.to_owned());
    d.put("previous-scan.json", &stray);
    assert_eq!(reason(&d.changes()), Some(Reason::OutOfStep));
    // Two at once: the earlier wins.
    stray.cancelled = true;
    d.put("previous-scan.json", &stray);
    assert_eq!(reason(&d.changes()), Some(Reason::Cancelled));

    // A previous report of an older version that decodes is compared.
    let mut older = b;
    older.schema_version = 7;
    d.put("previous-scan.json", &older);
    let r = d.changes();
    assert_eq!(reason(&r), None);
    assert_eq!(item(&r, "/h/p/target").delta, G as i64);
}

#[test]
fn a_folder_in_the_place_of_a_report_is_an_error_not_a_reason() {
    let d = Data::new();
    fs::create_dir(d.env.data_dir().join("last-scan.json")).unwrap();
    let e = changes::load(&d.env).unwrap_err();
    assert!(e.to_string().contains("last-scan.json"), "{e}");
}

/// The committed schema of `ddc changes --json` is the one the types give.
#[test]
#[allow(clippy::disallowed_methods)] // DDC_UPDATE_SCHEMA regenerates the committed file
fn committed_changes_schema_is_current() {
    let schema = serde_json::to_string_pretty(&schemars::schema_for!(ChangesReport)).unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/changes.schema.json");
    if std::env::var_os("DDC_UPDATE_SCHEMA").is_some() {
        fs::write(&path, &schema).unwrap();
    }
    let committed = fs::read_to_string(&path).unwrap_or_default();
    assert!(committed == schema, "docs/changes.schema.json is stale: run DDC_UPDATE_SCHEMA=1 cargo test");
}
