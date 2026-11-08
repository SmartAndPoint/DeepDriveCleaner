//! The walk enters a plain btrfs subvolume and never a snapshot (safety
//! ADR, amendment of 2026-11-02, "btrfs and ZFS on Linux"; story "btrfs and
//! ZFS on Linux: subvolumes, datasets, snapshots and stable identities",
//! DDC-010), on the real mounts of the Gate's linux-mounts job. Elsewhere
//! every test here skips, in the environment group of
//! `.config/native-skips/linux.txt`.
#![cfg(target_os = "linux")]

mod common;

use std::path::Path;

use common::{Mount, TreeBuilder};
use ddc_core::scan::{self, DirCtx, DirListing, Post, Progress, SkipReason, Tag, Visitor, WalkOptions};

/// Claims every folder named `node_modules`, as the catalog does.
struct NodeModules;

impl Visitor for NodeModules {
    fn after_read(&self, dir: &DirCtx<'_>, _listing: &DirListing) -> Post {
        if dir.name == "node_modules" { Post::Claim(1 as Tag) } else { Post::Descend }
    }
}

fn walk(root: &Path) -> scan::WalkOutput {
    scan::walk(&[root.to_path_buf()], &NodeModules, &WalkOptions::default(), &Progress::default()).expect("walk")
}

fn skipped(out: &scan::WalkOutput, path: &Path) -> Option<SkipReason> {
    out.skipped.iter().find(|s| s.path == path).map(|s| s.reason)
}

/// A plain subvolume and one nested in it are entered and counted, and
/// listed as `holds-subvolume`; a finding inside them, holding none, is
/// complete. Snapshots (read-only, writable, received) are `snapshot`, never
/// entered; another btrfs mounted inside stays `other-filesystem`.
#[test]
fn a_plain_subvolume_is_entered_and_a_snapshot_never() {
    let Some(t) = common::mount("a_plain_subvolume_is_entered_and_a_snapshot_never", Mount::BtrfsTree) else {
        return;
    };
    let out = walk(&t);
    assert_eq!(skipped(&out, &t.join("work")), Some(SkipReason::HoldsSubvolume), "{:#?}", out.skipped);
    assert_eq!(skipped(&out, &t.join("work/nested")), Some(SkipReason::HoldsSubvolume), "the parent's device decides");
    for snap in ["snap-ro", "snap-rw", "recv/snap-ro"] {
        assert_eq!(skipped(&out, &t.join(snap)), Some(SkipReason::Snapshot), "{snap}");
    }
    assert_eq!(skipped(&out, &t.join("data")), Some(SkipReason::OtherFilesystem), "another btrfs mounted inside");

    let owner = |rel: &str| out.owners.iter().find(|o| o.path == t.join(rel)).unwrap_or_else(|| panic!("{rel}"));
    let app = owner("work/app/node_modules");
    assert!(!app.incomplete(), "a finding inside a subvolume, holding none, is complete: {app:?}");
    assert!(app.bytes >= 65_536, "counted: {}", app.bytes);
    assert!(!out.owners.iter().any(|o| o.path.starts_with(t.join("snap-ro")) || o.path.starts_with(t.join("recv"))));
    let root = out.roots.iter().find(|r| r.path == t).unwrap();
    assert!(root.bytes >= 2 * 65_536, "the subvolumes' files count: {}", root.bytes);
}

/// A `node_modules` that is a subvolume, and one that holds one: counted,
/// but incomplete, so never planned.
#[test]
fn a_finding_that_is_or_holds_a_subvolume_is_incomplete() {
    let Some(t) = common::mount("a_finding_that_is_or_holds_a_subvolume_is_incomplete", Mount::BtrfsTree) else {
        return;
    };
    let out = walk(&t);
    for rel in ["whole/node_modules", "holds/node_modules"] {
        let o = out.owners.iter().find(|o| o.path == t.join(rel)).unwrap_or_else(|| panic!("{rel}"));
        assert!(o.incomplete(), "{rel}: {o:?}");
        assert!(o.bytes > 0, "{rel} is counted");
    }
}

/// A finding inside a subvolume is cleaned like any other: its parent opens
/// beneath the scan root, as no mount lies on the way. The subvolume is made
/// here, in a tree of this test's own on the btrfs scan root.
#[test]
fn a_finding_inside_a_subvolume_is_deleted() {
    use std::fs::File;
    use std::time::{Duration, SystemTime};

    use common::fakes::{FakeCommands, FakeProcs, FakeTrash};
    use ddc_core::class::RiskClass;
    use ddc_core::env::Env;
    use ddc_core::exec::{self, Consent, ExecOptions, MemoryJournal, Outcome, Ports};
    use ddc_core::plan::{PlanOptions, build_plan};
    use ddc_core::report::{self, ScanOptions};
    use ddc_core::rules::Catalog;

    let Some(t) = common::mount("a_finding_inside_a_subvolume_is_deleted", Mount::BtrfsTree) else {
        return;
    };
    let tree = TreeBuilder::new_in(&t);
    let made =
        std::process::Command::new("btrfs").args(["subvolume", "create"]).arg(tree.path("sv")).output().expect("btrfs");
    assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
    let now = SystemTime::now();
    tree.write("sv/p/package.json", "{}");
    tree.write("sv/p/package-lock.json", "{}");
    tree.file("sv/p/node_modules/dep/index.js", 20_000);
    for f in ["sv/p/package.json", "sv/p/package-lock.json"] {
        File::options()
            .write(true)
            .open(tree.path(f))
            .unwrap()
            .set_modified(now - Duration::from_secs(86_400 * 60))
            .unwrap();
    }
    let data = tempfile::tempdir().unwrap();
    let env = Env::with_paths(tree.root(), data.path(), &data.path().join("logs"), now).unwrap();
    let scanned = report::scan(&env, &ScanOptions::default(), &Progress::default()).unwrap();
    let opts = PlanOptions { permanent: true, ..PlanOptions::default() };
    let plan = build_plan(&scanned, &Catalog::for_home(tree.root()), &opts);
    assert!(plan.items.iter().any(|i| i.finding.path == tree.path("sv/p/node_modules")), "{plan:#?}");

    let consent = Consent::AssumeYes { classes: vec![RiskClass::Safe, RiskClass::Rebuildable] };
    let trash = FakeTrash(data.path().join("FakeTrash"));
    let mut journal = MemoryJournal::default();
    let ports = Ports {
        commands: &FakeCommands::default(),
        trash: &trash,
        procs: &FakeProcs::new(vec![]),
        journal: &mut journal,
    };
    let report =
        exec::execute(&env, &Catalog::for_home(tree.root()), &plan, &consent, ports, &ExecOptions::default()).unwrap();
    let nm = report.items.iter().find(|i| i.path == tree.path("sv/p/node_modules")).expect("planned");
    assert_eq!(nm.outcome, Outcome::Deleted, "{report:#?}");
    assert!(!tree.path("sv/p/node_modules").exists());
    assert!(tree.path("sv/p/package.json").exists());
}

/// A finding that is itself a subvolume is refused by the guard on its own,
/// even from a plan that calls it complete: the folder's device differs
/// from its parent's. Nothing is removed.
#[test]
fn the_guard_refuses_a_finding_that_is_a_subvolume_whatever_the_plan_says() {
    use std::fs::File;
    use std::time::{Duration, SystemTime};

    use common::fakes::{FakeCommands, FakeProcs, FakeTrash};
    use ddc_core::class::RiskClass;
    use ddc_core::env::Env;
    use ddc_core::exec::{self, Consent, ExecOptions, MemoryJournal, Outcome, Ports, SkipReason as Skip};
    use ddc_core::plan::{PlanOptions, build_plan};
    use ddc_core::report::{self, ScanOptions};
    use ddc_core::rules::Catalog;

    let Some(t) =
        common::mount("the_guard_refuses_a_finding_that_is_a_subvolume_whatever_the_plan_says", Mount::BtrfsTree)
    else {
        return;
    };
    let tree = TreeBuilder::new_in(&t);
    let now = SystemTime::now();
    tree.write("p/package.json", "{}");
    tree.write("p/package-lock.json", "{}");
    let made = std::process::Command::new("btrfs")
        .args(["subvolume", "create"])
        .arg(tree.path("p/node_modules"))
        .output()
        .expect("btrfs");
    assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
    tree.file("p/node_modules/dep/index.js", 20_000);
    for f in ["p/package.json", "p/package-lock.json"] {
        File::options()
            .write(true)
            .open(tree.path(f))
            .unwrap()
            .set_modified(now - Duration::from_secs(86_400 * 60))
            .unwrap();
    }
    let data = tempfile::tempdir().unwrap();
    let env = Env::with_paths(tree.root(), data.path(), &data.path().join("logs"), now).unwrap();
    let mut scanned = report::scan(&env, &ScanOptions::default(), &Progress::default()).unwrap();
    let nm = tree.path("p/node_modules");
    let finding = scanned.findings.iter_mut().find(|f| f.path == nm).expect("found");
    assert!(finding.incomplete, "the scan marks it");
    finding.incomplete = false;
    let opts = PlanOptions { permanent: true, ..PlanOptions::default() };
    let plan = build_plan(&scanned, &Catalog::for_home(tree.root()), &opts);
    assert!(plan.items.iter().any(|i| i.finding.path == nm), "{plan:#?}");

    let consent = Consent::AssumeYes { classes: vec![RiskClass::Safe, RiskClass::Rebuildable] };
    let trash = FakeTrash(data.path().join("FakeTrash"));
    let mut journal = MemoryJournal::default();
    let ports = Ports {
        commands: &FakeCommands::default(),
        trash: &trash,
        procs: &FakeProcs::new(vec![]),
        journal: &mut journal,
    };
    let report =
        exec::execute(&env, &Catalog::for_home(tree.root()), &plan, &consent, ports, &ExecOptions::default()).unwrap();
    let item = report.items.iter().find(|i| i.path == nm).expect("planned");
    assert_eq!(item.outcome, Outcome::Skipped(Skip::Incomplete), "{report:#?}");
    assert!(tree.path("p/node_modules/dep/index.js").exists(), "nothing removed");
}

/// Every step of the enter rule for `work`, with its values in the message:
/// what a real btrfs on the runner's kernel answers, where no other machine
/// has one.
#[test]
fn the_subvolume_rule_tells_each_step_for_work() {
    let Some(t) = common::mount("the_subvolume_rule_tells_each_step_for_work", Mount::BtrfsTree) else {
        return;
    };
    let trace = ddc_core::platform::bulk::subvolume_trace(&t, std::ffi::OsStr::new("work"));
    assert!(trace.ends_with("edge: Subvolume"), "{trace}");
}

/// A file held open inside a subvolume is in use: one identity for the
/// guard, the walk and the process table, its device the subvolume's
/// `f_fsid` folded (safety ADR, amendment of 2026-11-02, "Stable
/// identities").
#[test]
fn a_file_held_open_inside_a_subvolume_is_in_use() {
    let Some(t) = common::mount("a_file_held_open_inside_a_subvolume_is_in_use", Mount::BtrfsTree) else {
        return;
    };
    let tree = TreeBuilder::new_in(&t);
    let made =
        std::process::Command::new("btrfs").args(["subvolume", "create"]).arg(tree.path("sv")).output().expect("btrfs");
    assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
    common::mounts::held_open_is_in_use("a_file_held_open_inside_a_subvolume_is_in_use", &tree.file("sv/held", 4096));
}

/// A scan saved before the btrfs under it was mounted again, with another
/// anonymous device for the subvolume `home` (`scripts/ci-mounts.sh`),
/// still matches: its finding is deleted, never `changed-since-scan`, and
/// the identity it recorded for a kept folder removes that folder's
/// `.ddc-keep`, as `ddc keep remove --folder-dev --folder-ino` does.
#[test]
fn a_saved_scan_and_a_keep_record_still_match_after_a_remount() {
    use std::os::unix::fs::MetadataExt as _;
    use std::time::SystemTime;

    use common::fakes::{FakeCommands, FakeProcs, FakeTrash};
    use ddc_core::class::RiskClass;
    use ddc_core::env::Env;
    use ddc_core::exec::{self, Consent, ExecOptions, MemoryJournal, MemoryKeepJournal, Outcome, Ports};
    use ddc_core::keep::{self, KeepResult, Target};
    use ddc_core::plan::{PlanOptions, build_plan};
    use ddc_core::platform::meta;
    use ddc_core::rules::Catalog;
    use ddc_core::store;

    let test = "a_saved_scan_and_a_keep_record_still_match_after_a_remount";
    let Some(home) = common::mount(test, Mount::BtrfsRemount) else {
        return;
    };
    let state = home.parent().unwrap().join("state");
    let before: u64 = std::fs::read_to_string(state.join("before-dev")).unwrap().trim().parse().unwrap();
    assert_ne!(std::fs::symlink_metadata(&home).unwrap().dev(), before, "the remount moved the anonymous device");

    let env = Env::with_paths(&home, &state.join("data"), &state.join("logs"), SystemTime::now()).unwrap();
    let saved = store::read_last_scan(&env).unwrap().expect("the scan made before the remount");
    let found = |rel: &str| {
        let path = home.join(rel);
        saved.findings.iter().find(|f| f.path == path).unwrap_or_else(|| panic!("{rel}: {:#?}", saved.findings))
    };
    let nm = found("p/node_modules");
    assert_ne!(nm.dev, before, "the scan recorded no anonymous device");
    assert_eq!((nm.dev, nm.ino), meta::lstat(&nm.path).unwrap().id());

    let opts = PlanOptions { permanent: true, ..PlanOptions::default() };
    let plan = build_plan(&saved, &Catalog::for_home(&home), &opts);
    assert!(plan.items.iter().any(|i| i.finding.path == nm.path), "{plan:#?}");
    let consent = Consent::AssumeYes { classes: vec![RiskClass::Safe, RiskClass::Rebuildable] };
    let data = tempfile::tempdir().unwrap();
    let trash = FakeTrash(data.path().join("FakeTrash"));
    let mut journal = MemoryJournal::default();
    let ports = Ports {
        commands: &FakeCommands::default(),
        trash: &trash,
        procs: &FakeProcs::new(vec![]),
        journal: &mut journal,
    };
    let report =
        exec::execute(&env, &Catalog::for_home(&home), &plan, &consent, ports, &ExecOptions::default()).unwrap();
    let item = report.items.iter().find(|i| i.path == nm.path).expect("planned");
    assert_eq!(item.outcome, Outcome::Deleted, "{report:#?}");
    assert!(!nm.path.exists());

    let kept = found("q/node_modules");
    let marker = kept.protection.as_ref().and_then(|p| p.markers.iter().find(|m| m.folder == kept.path));
    let marker = marker.unwrap_or_else(|| panic!("no marker of its own: {kept:#?}"));
    let target = Target { folder: kept.path.clone(), identity: Some((marker.dev, marker.ino)), rule: None };
    let removed = keep::unprotect(&target, std::slice::from_ref(&home), &mut MemoryKeepJournal::default()).unwrap();
    assert_eq!(removed.result, KeepResult::Removed, "{removed:#?}");
    assert!(!kept.path.join(".ddc-keep").exists());
}
