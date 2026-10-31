//! Story "Faster clean: re-check only what the selection needs", AC 6 and
//! gate decisions 5 and 6: `last-scan.json` after a clean is the report the
//! CLI read, edited by what the clean did. Temporary homes only; nothing is
//! cleaned here: the clean's outcomes are written by hand.

mod common;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::{OLD, TreeBuilder};
use ddc_core::class::RiskClass;
use ddc_core::env::Env;
use ddc_core::exec::report::{CLEAN_SCHEMA_VERSION, CleanItem, CleanReport};
use ddc_core::exec::{Outcome, SkipReason, VolumeSpace};
use ddc_core::model::{Finding, FolderEntry, ScanReport};
use ddc_core::plan::ActionKind;
use ddc_core::report::after_clean::{self, Measure, Remeasured};
use ddc_core::report::contents::ContentsLimits;
use ddc_core::report::{self, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::scan::{Progress, WalkOptions};

struct Home {
    t: TreeBuilder,
    data: tempfile::TempDir,
    limits: ContentsLimits,
}

impl Home {
    fn new() -> Self {
        Self {
            t: TreeBuilder::new(),
            data: tempfile::tempdir().unwrap(),
            limits: ContentsLimits { max: 100, min_bytes: 1 },
        }
    }

    fn env(&self) -> Env {
        // An hour on: entries are not "recent".
        let now = SystemTime::now() + Duration::from_secs(3_600);
        Env::with_paths(self.t.root(), self.data.path(), &self.data.path().join("logs"), now).unwrap()
    }

    fn opts(&self) -> ScanOptions {
        ScanOptions {
            contents: self.limits,
            walk: WalkOptions { threads: Some(2), ..WalkOptions::default() },
            ..ScanOptions::default()
        }
    }

    fn scan(&self) -> ScanReport {
        report::scan(&self.env(), &self.opts(), &Progress::default()).unwrap()
    }

    /// `report` edited by a clean of `items`.
    fn edited(&self, report: &ScanReport, items: Vec<CleanItem>, remeasured: &[(&Path, Remeasured)]) -> ScanReport {
        let remeasured: HashMap<PathBuf, Remeasured> = remeasured.iter().map(|(p, m)| (p.to_path_buf(), *m)).collect();
        let mut edited = report.clone();
        after_clean::edit(&mut edited, &clean(items), None, &remeasured, &self.opts());
        edited
    }
}

fn finding<'a>(report: &'a ScanReport, path: &Path) -> &'a Finding {
    report.findings.iter().find(|f| f.path == path).unwrap_or_else(|| panic!("no finding at {path:?}"))
}

/// A report's RFC 3339 time as Unix seconds.
fn secs(rfc3339: &str) -> i64 {
    let t = humantime::parse_rfc3339(rfc3339).unwrap();
    t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

fn has(report: &ScanReport, path: &Path) -> bool {
    report.findings.iter().any(|f| f.path == path)
}

fn entry<'a>(report: &'a ScanReport, folder: &Path, path: &Path) -> Option<&'a FolderEntry> {
    finding(report, folder).contents.as_ref().unwrap().entries.iter().find(|e| e.path == path)
}

/// A line of the clean report; the edit reads its path, figures and outcome.
fn item(path: &Path, rule: &str, bytes: Option<u64>, outcome: Outcome) -> CleanItem {
    CleanItem {
        path: path.to_path_buf(),
        dev: 0,
        ino: 0,
        rule: rule.to_owned(),
        class: RiskClass::Safe,
        action: ActionKind::Delete,
        permanent: false,
        bytes,
        outcome,
    }
}

fn clean(items: Vec<CleanItem>) -> CleanReport {
    CleanReport {
        schema_version: CLEAN_SCHEMA_VERSION,
        ddc_version: "test".to_owned(),
        platform: ddc_core::platform::PLATFORM,
        capabilities: ddc_core::platform::CAPABILITIES.to_vec(),
        cancelled: false,
        items,
        failed: 0,
        volumes: Vec::new(),
        local_snapshots: 0,
        journal: PathBuf::from("/nonexistent/actions.jsonl"),
        last_scan_saved: false,
        lossy_paths: None,
    }
}

/// The finding-derived totals agree with the findings, as the scan computes them.
fn check_totals(r: &ScanReport) {
    let shown: Vec<&Finding> = r.findings.iter().filter(|f| !f.ignored).collect();
    for c in &r.totals.classes {
        let of: Vec<&&Finding> = shown.iter().filter(|f| f.class == c.class).collect();
        let selected: Vec<&&&Finding> = of.iter().filter(|f| f.selected).collect();
        assert_eq!(c.count, of.len() as u64, "{:?}", c.class);
        assert_eq!(c.bytes, of.iter().map(|f| f.bytes).sum::<u64>(), "{:?}", c.class);
        assert_eq!(c.selected_count, selected.len() as u64, "{:?}", c.class);
        assert_eq!(c.selected_bytes, selected.iter().map(|f| f.bytes).sum::<u64>(), "{:?}", c.class);
    }
    let selected = shown.iter().filter(|f| f.selected);
    assert_eq!(r.totals.selected_bytes, selected.clone().map(|f| f.bytes).sum::<u64>());
    assert_eq!(r.totals.selected_reclaimable_bytes, selected.map(|f| f.reclaimable_bytes()).sum::<u64>());
    let ignored: Vec<&Finding> = r.findings.iter().filter(|f| f.ignored).collect();
    assert_eq!(r.totals.ignored_count, ignored.len() as u64);
    assert_eq!(r.totals.ignored_bytes, ignored.iter().map(|f| f.bytes).sum::<u64>());
    let mut sorted = r.findings.clone();
    sorted.sort_by(|a, b| a.class.cmp(&b.class).then(b.bytes.cmp(&a.bytes)).then(a.path.cmp(&b.path)));
    assert_eq!(sorted, r.findings, "in the report's order");
}

#[test]
fn a_deleted_finding_leaves_with_the_reports_own_figures() {
    if common::unsupported("a_deleted_finding_leaves_with_the_reports_own_figures") {
        return;
    }
    let home = Home::new();
    home.t.file("q/__pycache__/a/m.pyc", 10_000);
    home.t.file("p/__pycache__/n.pyc", 3_000);
    let before = home.scan();
    let path = home.t.path("q/__pycache__");
    let f = finding(&before, &path).clone();

    // The clean's own measure is never what leaves.
    let after = home.edited(&before, vec![item(&path, "python.pycache", Some(1), Outcome::Deleted)], &[]);

    assert!(!has(&after, &path));
    assert!(has(&after, &home.t.path("p/__pycache__")));
    assert_eq!(after.totals.scanned_bytes, before.totals.scanned_bytes - f.bytes);
    assert_eq!(after.totals.files, before.totals.files - f.files);
    assert_eq!(after.totals.dirs, before.totals.dirs - f.dirs);
    assert_eq!(after.totals.unowned_bytes, before.totals.unowned_bytes);
    assert_eq!((&after.generated_at, after.duration_ms), (&before.generated_at, before.duration_ms));
    check_totals(&after);
}

#[test]
fn skipped_failed_and_not_run_items_stay_and_an_item_the_report_does_not_hold_edits_nothing() {
    if common::unsupported("skipped_failed_and_not_run_items_stay_and_an_item_the_report_does_not_hold_edits_nothing") {
        return;
    }
    let home = Home::new();
    home.t.file("q/__pycache__/m.pyc", 10_000);
    home.t.file(".Trash/old.bin", 4_000);
    home.t.file("Downloads/kept.bin", 4_000);
    let before = home.scan();
    let path = home.t.path("q/__pycache__");
    let elsewhere = home.t.path("w/__pycache__");
    let items = vec![
        item(&path, "python.pycache", Some(10_000), Outcome::Skipped(SkipReason::Kept)),
        item(&path, "python.pycache", Some(10_000), Outcome::Failed { error: "EPERM".to_owned() }),
        item(&path, "python.pycache", None, Outcome::NotRun),
        item(&path, "python.pycache", None, Outcome::Native { commands: Vec::new() }),
        // A newer scan's report, written in between, does not hold these.
        item(&elsewhere, "python.pycache", Some(5_000), Outcome::Deleted),
        item(&elsewhere, "python.pycache", Some(5_000), Outcome::Trashed),
        item(&home.t.path("Elsewhere/x.bin"), "user.downloads", Some(5_000), Outcome::Trashed),
    ];
    assert_eq!(home.edited(&before, items, &[(&path, Remeasured::Unknown)]), before);
}

/// Story "Review window: after a clean, only skips that can clear stay
/// selected": with the re-check's walk at hand, a skipped or not-run item
/// takes its finding from there, or leaves when it is gone from the disk;
/// a failed one and a picked entry stay as they were.
#[test]
fn skipped_and_not_run_items_take_the_rechecks_finding_or_leave() {
    if common::unsupported("skipped_and_not_run_items_take_the_rechecks_finding_or_leave") {
        return;
    }
    let home = Home::new();
    home.t.file("a/__pycache__/m.pyc", 10_000);
    home.t.file("b/__pycache__/m.pyc", 10_000);
    home.t.file("c/__pycache__/m.pyc", 10_000);
    home.t.file("d/__pycache__/m.pyc", 10_000);
    home.t.file("Downloads/kept.bin", 4_000);
    home.t.age("", OLD);
    let before = home.scan();
    let [a, b, c, d] = ["a", "b", "c", "d"].map(|p| home.t.path(&format!("{p}/__pycache__")));
    // `a` swapped for a larger folder, `b` gone, `c` and `d` swapped too.
    for p in [&a, &b, &c, &d] {
        fs::rename(p, p.with_file_name("old")).unwrap();
    }
    home.t.file("a/__pycache__/m.pyc", 30_000);
    home.t.file("c/__pycache__/m.pyc", 10_000);
    home.t.file("d/__pycache__/m.pyc", 10_000);
    for p in ["a", "c", "d"] {
        home.t.age(&format!("{p}/__pycache__"), OLD + 60);
    }
    let fresh = home.scan();
    let picked = home.t.path("Downloads/kept.bin");
    let items = vec![
        item(&a, "python.pycache", Some(10_000), Outcome::Skipped(SkipReason::ChangedSinceScan)),
        item(&b, "python.pycache", Some(10_000), Outcome::Skipped(SkipReason::Missing)),
        item(&c, "python.pycache", None, Outcome::NotRun),
        item(&d, "python.pycache", Some(10_000), Outcome::Failed { error: "EPERM".to_owned() }),
        item(&picked, "user.downloads", Some(4_000), Outcome::Skipped(SkipReason::ChangedSinceScan)),
    ];

    let mut after = before.clone();
    after_clean::edit(&mut after, &clean(items), Some(&fresh), &HashMap::new(), &home.opts());

    assert_eq!(finding(&after, &a), finding(&fresh, &a), "the re-check's identity and size");
    // Story "Review window: sort a group's rows by name, size or last change":
    // and the re-check's date.
    assert_eq!(finding(&before, &a).modified.as_deref().map(secs), Some(OLD));
    assert_eq!(finding(&after, &a).modified.as_deref().map(secs), Some(OLD + 60));
    assert_eq!(finding(&after, &c).modified.as_deref().map(secs), Some(OLD + 60));
    assert_eq!(finding(&after, &d).modified.as_deref().map(secs), Some(OLD), "untouched: the report's own");
    assert!(!has(&after, &b), "gone from the disk, gone from the report");
    assert_eq!(finding(&after, &c), finding(&fresh, &c));
    assert_eq!(finding(&after, &d), finding(&before, &d), "a failed action is measured by the next scan");
    let downloads = home.t.path("Downloads");
    assert_eq!(finding(&after, &downloads), finding(&before, &downloads), "a picked entry is not a finding");
    let (fa, fb) = (finding(&fresh, &a), finding(&before, &b));
    let old_a = finding(&before, &a);
    assert_eq!(after.totals.scanned_bytes, before.totals.scanned_bytes + fa.bytes - old_a.bytes - fb.bytes);
    assert_eq!(after.totals.files, before.totals.files + fa.files - old_a.files - fb.files);
    check_totals(&after);

    // Without the re-check's walk (the report is the rescan's own), nothing moves.
    let mut same = before.clone();
    let items = vec![item(&a, "python.pycache", Some(10_000), Outcome::Skipped(SkipReason::ChangedSinceScan))];
    after_clean::edit(&mut same, &clean(items), None, &HashMap::new(), &home.opts());
    assert_eq!(same, before);
}

#[test]
fn a_trashed_finding_moves_to_the_trash_of_its_volume_or_counts_as_deleted() {
    if common::unsupported("a_trashed_finding_moves_to_the_trash_of_its_volume_or_counts_as_deleted") {
        return;
    }
    let home = Home::new();
    home.t.file("q/__pycache__/m.pyc", 10_000);
    home.t.file(".Trash/old.bin", 4_000);
    let before = home.scan();
    let path = home.t.path("q/__pycache__");
    let trash = home.t.path(".Trash");
    let (f, t) = (finding(&before, &path).clone(), finding(&before, &trash).clone());

    let after = home.edited(&before, vec![item(&path, "python.pycache", Some(10_000), Outcome::Trashed)], &[]);

    assert!(!has(&after, &path));
    let moved = finding(&after, &trash);
    assert_eq!((moved.bytes, moved.files, moved.dirs), (t.bytes + f.bytes, t.files + f.files, t.dirs + f.dirs));
    assert_eq!(
        (after.totals.scanned_bytes, after.totals.files, after.totals.dirs),
        (before.totals.scanned_bytes, before.totals.files, before.totals.dirs),
        "the bytes stay under the scan roots"
    );
    let review = |r: &ScanReport| r.totals.classes.iter().find(|c| c.class == RiskClass::Review).unwrap().bytes;
    assert_eq!(review(&after), review(&before) + f.bytes);
    check_totals(&after);

    // An empty or never scanned Trash: the Trash reads low until the next full scan.
    let mut no_trash = before.clone();
    no_trash.findings.retain(|f| f.path != trash);
    let after = home.edited(&no_trash, vec![item(&path, "python.pycache", Some(10_000), Outcome::Trashed)], &[]);
    assert_eq!(after.totals.scanned_bytes, before.totals.scanned_bytes - f.bytes);
    assert_eq!(after.totals.files, before.totals.files - f.files);
}

#[test]
fn a_tool_cleaned_item_is_at_its_new_size_or_gone() {
    if common::unsupported("a_tool_cleaned_item_is_at_its_new_size_or_gone") {
        return;
    }
    let home = Home::new();
    home.t.file(".cache/uv/wheels/a.whl", 200_000);
    home.t.file(".cache/uv/wheels/b.whl", 100_000);
    let before = home.scan();
    let path = home.t.path(".cache/uv");
    let old = finding(&before, &path).clone();
    let small = Measure {
        bytes: 4_096,
        shared_bytes: 0,
        private_bytes: Some(4_096),
        files: 1,
        dirs: 1,
        incomplete: false,
        modified: Some(OLD + 70),
    };
    let native = || vec![item(&path, "cache.uv", Some(old.bytes), Outcome::Native { commands: Vec::new() })];

    let after = home.edited(&before, native(), &[(&path, Remeasured::Measured(small))]);
    let f = finding(&after, &path);
    assert_eq!((f.bytes, f.private_bytes, f.files, f.dirs), (4_096, Some(4_096), 1, 1));
    // Story "Review window: sort a group's rows by name, size or last change":
    // the re-measure's date, fixed-width UTC.
    assert_ne!(old.modified, f.modified);
    assert_eq!(f.modified.as_deref(), Some("2024-01-01T00:01:10Z"));
    let undated = home.edited(&before, native(), &[(&path, Remeasured::Measured(Measure { modified: None, ..small }))]);
    assert_eq!(finding(&undated, &path).modified, None, "a re-measure without a date leaves none");
    assert_eq!(after.totals.scanned_bytes, before.totals.scanned_bytes - old.bytes + 4_096);
    assert_eq!(after.totals.files, before.totals.files - old.files + 1);
    assert_eq!(after.totals.dirs, before.totals.dirs - old.dirs + 1);
    check_totals(&after);

    // Grown: the totals follow it up.
    let big = Measure { bytes: old.bytes * 3, ..small };
    let after = home.edited(&before, native(), &[(&path, Remeasured::Measured(big))]);
    assert_eq!(after.totals.scanned_bytes, before.totals.scanned_bytes + old.bytes * 2);

    // Under `min_size` now: hidden, and in no class total.
    let mut opts = home.opts();
    opts.min_size = 8_192;
    let mut hidden = before.clone();
    let remeasured = HashMap::from([(path.clone(), Remeasured::Measured(small))]);
    after_clean::edit(&mut hidden, &clean(native()), None, &remeasured, &opts);
    let f = finding(&hidden, &path);
    assert!(f.ignored && !f.selected);
    assert_eq!((hidden.totals.ignored_count, hidden.totals.ignored_bytes), (1, 4_096));
    check_totals(&hidden);

    let gone = home.edited(&before, native(), &[(&path, Remeasured::Gone)]);
    assert!(!has(&gone, &path));
    assert_eq!(gone.totals.files, before.totals.files - old.files);
    check_totals(&gone);
}

#[test]
fn a_picked_folder_leaves_its_folder_with_the_findings_inside_it() {
    if common::unsupported("a_picked_folder_leaves_its_folder_with_the_findings_inside_it") {
        return;
    }
    let home = Home::new();
    home.t.file("Downloads/picked/a.bin", 30_000);
    home.t.file("Downloads/picked/__pycache__/m.pyc", 10_000);
    home.t.file("Downloads/other/c.bin", 20_000);
    home.t.file(".Trash/old.bin", 4_000);
    let before = home.scan();
    let (downloads, picked, inner) =
        (home.t.path("Downloads"), home.t.path("Downloads/picked"), home.t.path("Downloads/picked/__pycache__"));
    let trash = home.t.path(".Trash");
    let e = entry(&before, &downloads, &picked).unwrap().clone();
    assert_eq!(e.findings, 1);
    let inner_bytes = finding(&before, &inner).bytes;

    let items = vec![
        item(&picked, "user.downloads", Some(e.bytes), Outcome::Trashed),
        // Listed too, and left with the entry that held it.
        item(&inner, "python.pycache", None, Outcome::Skipped(SkipReason::InsideAnotherItem)),
    ];
    let after = home.edited(&before, items, &[]);

    assert!(entry(&after, &downloads, &picked).is_none());
    assert!(entry(&after, &downloads, &home.t.path("Downloads/other")).is_some());
    assert!(!has(&after, &inner), "the finding inside left with it");
    assert_eq!(finding(&after, &downloads).bytes, finding(&before, &downloads).bytes - (e.bytes - inner_bytes));
    assert_eq!(finding(&after, &trash).bytes, finding(&before, &trash).bytes + e.bytes);
    assert_eq!(finding(&after, &trash).files, finding(&before, &trash).files, "a folder's files are not known");
    assert_eq!(
        (after.totals.scanned_bytes, after.totals.files, after.totals.dirs),
        (before.totals.scanned_bytes, before.totals.files, before.totals.dirs)
    );
    check_totals(&after);
}

#[test]
fn a_picked_file_without_a_trash_finding_counts_as_deleted() {
    if common::unsupported("a_picked_file_without_a_trash_finding_counts_as_deleted") {
        return;
    }
    let home = Home::new();
    home.t.file("Downloads/big.bin", 30_000);
    home.t.file("Downloads/other.bin", 20_000);
    let before = home.scan();
    let (downloads, picked) = (home.t.path("Downloads"), home.t.path("Downloads/big.bin"));
    let e = entry(&before, &downloads, &picked).unwrap().clone();

    let after = home.edited(&before, vec![item(&picked, "user.downloads", Some(e.bytes), Outcome::Trashed)], &[]);

    assert!(entry(&after, &downloads, &picked).is_none());
    assert_eq!(after.totals.scanned_bytes, before.totals.scanned_bytes - e.bytes);
    assert_eq!(after.totals.files, before.totals.files - 1, "a file is one file");
    assert_eq!(after.totals.dirs, before.totals.dirs);
    check_totals(&after);
}

#[test]
fn a_picked_entry_in_the_rest_leaves_with_the_cleans_measure_and_never_wraps_a_total() {
    if common::unsupported("a_picked_entry_in_the_rest_leaves_with_the_cleans_measure_and_never_wraps_a_total") {
        return;
    }
    let mut home = Home::new();
    home.limits = ContentsLimits { max: 1, min_bytes: 1 };
    home.t.file("Downloads/big/a.bin", 60_000);
    home.t.file("Downloads/small/b.bin", 8_000);
    home.t.file("Downloads/tiny/c.bin", 4_000);
    let before = home.scan();
    let (downloads, small) = (home.t.path("Downloads"), home.t.path("Downloads/small"));
    assert!(entry(&before, &downloads, &small).is_none(), "in the rest");
    let rest = |r: &ScanReport| {
        let c = finding(r, &downloads).contents.clone().unwrap();
        (c.rest_count, c.rest_bytes)
    };
    let (count, bytes) = rest(&before);

    let after = home.edited(&before, vec![item(&small, "user.downloads", Some(8_192), Outcome::Trashed)], &[]);
    assert_eq!(rest(&after), (count - 1, bytes - 8_192));
    assert_eq!(after.totals.scanned_bytes, before.totals.scanned_bytes - 8_192);
    assert_eq!(after.totals.files, before.totals.files, "its kind is not known in the rest");

    // It grew far past what the report holds: every total stops at zero.
    let grown = home.edited(&before, vec![item(&small, "user.downloads", Some(u64::MAX / 2), Outcome::Trashed)], &[]);
    assert_eq!(rest(&grown), (count - 1, 0));
    assert_eq!((finding(&grown, &downloads).bytes, grown.totals.scanned_bytes), (0, 0));
    check_totals(&grown);
}

#[test]
fn a_finding_inside_a_downloads_entry_shrinks_that_entry_or_the_rest() {
    if common::unsupported("a_finding_inside_a_downloads_entry_shrinks_that_entry_or_the_rest") {
        return;
    }
    let mut home = Home::new();
    home.limits = ContentsLimits { max: 1, min_bytes: 1 };
    home.t.file("Downloads/proj/src/a.bin", 60_000);
    home.t.file("Downloads/proj/__pycache__/m.pyc", 10_000);
    home.t.file("Downloads/small/__pycache__/n.pyc", 4_000);
    let before = home.scan();
    let downloads = home.t.path("Downloads");
    let (proj, listed) = (home.t.path("Downloads/proj"), home.t.path("Downloads/proj/__pycache__"));
    let in_rest = home.t.path("Downloads/small/__pycache__");
    let e = entry(&before, &downloads, &proj).unwrap().clone();
    let (a, b) = (finding(&before, &listed).bytes, finding(&before, &in_rest).bytes);
    let rest_bytes = |r: &ScanReport| finding(r, &downloads).contents.as_ref().unwrap().rest_bytes;

    let items = vec![
        item(&listed, "python.pycache", Some(a), Outcome::Deleted),
        item(&in_rest, "python.pycache", Some(b), Outcome::Deleted),
    ];
    let after = home.edited(&before, items, &[]);

    let shrunk = entry(&after, &downloads, &proj).unwrap();
    assert_eq!((shrunk.bytes, shrunk.findings), (e.bytes - a, e.findings - 1));
    assert_eq!(rest_bytes(&after), rest_bytes(&before) - b);
    assert_eq!(finding(&after, &downloads).bytes, finding(&before, &downloads).bytes, "the folder's own bytes");
    assert_eq!(after.totals.scanned_bytes, before.totals.scanned_bytes - a - b);
    check_totals(&after);
}

#[test]
fn lines_under_a_removed_path_leave_and_the_volumes_take_the_cleans_free_space() {
    if common::unsupported("lines_under_a_removed_path_leave_and_the_volumes_take_the_cleans_free_space") {
        return;
    }
    let home = Home::new();
    home.t.file("q/__pycache__/m.pyc", 10_000);
    home.t.file("q/__pycache__/locked/x.pyc", 1_000);
    home.t.file("r/locked/y.bin", 1_000);
    home.t.lock("q/__pycache__/locked");
    home.t.lock("r/locked");
    let before = home.scan();
    assert_eq!(before.inaccessible.len(), 2, "{:?}", before.inaccessible);
    let path = home.t.path("q/__pycache__");
    let mut done = clean(vec![item(&path, "python.pycache", Some(10_000), Outcome::Deleted)]);
    let mount = before.volumes[0].mount_point.clone();
    done.volumes = vec![
        VolumeSpace { mount_point: mount.clone(), free_before: 1, free_after: 123_456 },
        VolumeSpace { mount_point: PathBuf::from("/Volumes/Elsewhere"), free_before: 1, free_after: 2 },
    ];

    let mut after = before.clone();
    after_clean::edit(&mut after, &done, None, &HashMap::new(), &home.opts());

    assert_eq!(after.inaccessible.iter().map(|l| l.path.clone()).collect::<Vec<_>>(), [home.t.path("r/locked")]);
    assert_eq!(after.errors_total, before.errors_total - 1);
    assert_eq!(after.volumes.len(), before.volumes.len());
    assert_eq!(after.volumes[0].free_bytes, 123_456);
}

#[test]
fn a_tool_cleaned_item_is_measured_as_the_scan_measures_it() {
    if common::unsupported("a_tool_cleaned_item_is_measured_as_the_scan_measures_it") {
        return;
    }
    let home = Home::new();
    home.t.file(".cache/uv/wheels/a.whl", 200_000);
    home.t.file(".cache/uv/archive/b/c.bin", 50_000);
    let before = home.scan();
    let path = home.t.path(".cache/uv");
    let f = finding(&before, &path);
    let catalog = Catalog::builtin(home.t.root());
    let walk = home.opts().walk;

    let expected = Measure {
        bytes: f.bytes,
        shared_bytes: f.shared_bytes,
        private_bytes: f.private_bytes,
        files: f.files,
        dirs: f.dirs,
        incomplete: f.incomplete,
        modified: f.modified.as_deref().map(secs),
    };
    assert!(expected.modified.is_some(), "{f:?}");
    assert_eq!(after_clean::remeasure(&catalog, "cache.uv", &path, &walk), Remeasured::Measured(expected));
    let no_times = WalkOptions { claim_times: false, ..walk };
    match after_clean::remeasure(&catalog, "cache.uv", &path, &no_times) {
        Remeasured::Measured(m) => assert_eq!(m, Measure { modified: None, ..expected }),
        other => panic!("{other:?}"),
    }
    // A file: its own time, with the times on only.
    let file = home.t.file("loose.bin", 1_000);
    home.t.touch("loose.bin", OLD + 80);
    for (opts, modified) in [(&walk, Some(OLD + 80)), (&no_times, None)] {
        match after_clean::remeasure(&catalog, "cache.uv", &file, opts) {
            Remeasured::Measured(m) => assert_eq!(m.modified, modified),
            other => panic!("{other:?}"),
        }
    }

    let holding = home.data.path().join("holding");
    fs::create_dir_all(&holding).unwrap();
    fs::rename(path.join("archive"), holding.join("archive")).unwrap();
    match after_clean::remeasure(&catalog, "cache.uv", &path, &walk) {
        Remeasured::Measured(m) => assert_eq!((m.files, m.dirs), (1, 2)),
        other => panic!("{other:?}"),
    }
    fs::rename(&path, holding.join("uv")).unwrap();
    assert_eq!(after_clean::remeasure(&catalog, "cache.uv", &path, &walk), Remeasured::Gone);
    assert_eq!(after_clean::remeasure(&catalog, "no.such-rule", &holding, &walk), Remeasured::Unknown);
}
