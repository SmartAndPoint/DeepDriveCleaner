//! The walk enters a ZFS child dataset beneath an anchor of its own, and
//! never a snapshot or a mount that only looks like one (safety ADR,
//! amendment of 2026-11-02, "btrfs and ZFS on Linux"; story "btrfs and ZFS
//! on Linux: subvolumes, datasets, snapshots and stable identities",
//! DDC-010), on the real mounts of the Gate's linux-mounts job. Elsewhere
//! every test here skips, in the environment group of
//! `.config/native-skips/linux.txt`. A message that names an entry carries
//! the dataset rule's trace for it: which condition refused.
#![cfg(target_os = "linux")]

mod common;

use std::ffi::OsStr;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::{Mount, TreeBuilder};
use ddc_core::platform::Canonical as _;
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

/// The dataset rule for `rel`, a folder of `t`, step by step.
fn trace(t: &Path, rel: &str) -> String {
    let at = t.join(rel);
    let (dir, name) = (at.parent().unwrap(), at.file_name().unwrap());
    ddc_core::platform::bulk::dataset_trace(dir, name)
}

/// The lines of mountinfo whose mount point lies at or below `t`.
fn mounts_below(t: &Path) -> Vec<String> {
    let text = std::fs::read_to_string("/proc/self/mountinfo").expect("mountinfo");
    let mut lines: Vec<String> = text
        .lines()
        .filter(|l| l.split(' ').nth(4).is_some_and(|p| Path::new(&p.replace("\\040", " ")).starts_with(t)))
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

/// A child dataset and its grandchild are entered and counted, and listed
/// as `holds-subvolume`; a finding inside them, holding none, is complete.
/// A bind mount of a dataset's mount point, and a child mounted by a
/// `mountpoint=` of its own, stay `other-filesystem`.
#[test]
fn a_zfs_child_dataset_is_entered_and_a_bind_of_it_never() {
    let Some(t) = common::mount("a_zfs_child_dataset_is_entered_and_a_bind_of_it_never", Mount::ZfsTree) else {
        return;
    };
    let out = walk(&t);
    for rel in ["work", "work/nested", "snapdir"] {
        assert_eq!(skipped(&out, &t.join(rel)), Some(SkipReason::HoldsSubvolume), "{rel}\n{}", trace(&t, rel));
    }
    for rel in ["bound", "moved"] {
        assert_eq!(skipped(&out, &t.join(rel)), Some(SkipReason::OtherFilesystem), "{rel}\n{}", trace(&t, rel));
        assert!(!out.owners.iter().any(|o| o.path.starts_with(t.join(rel))), "{rel} is never walked");
    }
    assert!(trace(&t, "work").contains("anchor: Ok(Some("), "{}", trace(&t, "work"));

    let owner = |rel: &str| out.owners.iter().find(|o| o.path == t.join(rel)).unwrap_or_else(|| panic!("{rel}"));
    let app = owner("work/app/node_modules");
    assert!(!app.incomplete(), "a finding inside a dataset, holding none, is complete: {app:?}");
    assert!(app.bytes >= 65_536, "counted: {}", app.bytes);
    let root = out.roots.iter().find(|r| r.path == t).unwrap();
    assert!(root.bytes >= 2 * 65_536, "the datasets' files count: {}", root.bytes);
}

/// A `node_modules` that is a dataset, and one that holds one, each below a
/// dataset that is never mounted: counted, but incomplete, so never planned.
#[test]
fn a_finding_that_is_or_holds_a_dataset_is_incomplete() {
    let Some(t) = common::mount("a_finding_that_is_or_holds_a_dataset_is_incomplete", Mount::ZfsTree) else {
        return;
    };
    let out = walk(&t);
    for rel in ["whole/node_modules", "holds/node_modules/inner"] {
        assert_eq!(skipped(&out, &t.join(rel)), Some(SkipReason::HoldsSubvolume), "{rel}\n{}", trace(&t, rel));
    }
    for (rel, dataset) in
        [("whole/node_modules", "whole/node_modules"), ("holds/node_modules", "holds/node_modules/inner")]
    {
        let o = out.owners.iter().find(|o| o.path == t.join(rel)).unwrap_or_else(|| panic!("{rel}"));
        assert!(o.incomplete(), "{rel}: {o:?}\n{}", trace(&t, dataset));
        assert!(o.bytes > 0, "{rel} is counted");
    }
}

/// `.zfs` at a dataset with `snapdir=visible` is listed, `snapshot`, and
/// never entered: no snapshot is mounted by the scan.
#[test]
fn a_visible_snapshot_folder_is_never_entered_and_mounts_nothing() {
    let Some(t) = common::mount("a_visible_snapshot_folder_is_never_entered_and_mounts_nothing", Mount::ZfsTree) else {
        return;
    };
    let names: Vec<_> = std::fs::read_dir(t.join("snapdir")).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert!(names.iter().any(|n| n == OsStr::new(".zfs")), "snapdir=visible lists .zfs: {names:?}");
    let before = mounts_below(&t);
    let out = walk(&t);
    assert_eq!(mounts_below(&t), before, "the scan mounted something");
    assert_eq!(skipped(&out, &t.join("snapdir/.zfs")), Some(SkipReason::Snapshot), "{:#?}", out.skipped);
    let zfs = t.join("snapdir/.zfs");
    assert!(!out.skipped.iter().any(|s| s.path.starts_with(&zfs) && s.path != zfs), "nothing below .zfs is listed");
    assert!(!out.owners.iter().any(|o| o.path.starts_with(&zfs)));
}

/// Every condition of the dataset rule for `work`, with the mountinfo lines
/// in the message: what a real ZFS on the runner's kernel answers, where no
/// other machine has one.
#[test]
fn the_dataset_rule_tells_each_condition_for_work() {
    let Some(t) = common::mount("the_dataset_rule_tells_each_condition_for_work", Mount::ZfsTree) else {
        return;
    };
    let work = trace(&t, "work");
    assert!(work.contains("edge: Some(Dataset("), "{work}");
    assert!(!work.contains(": false"), "{work}");
    let bound = trace(&t, "bound");
    assert!(bound.contains("mount point <parent mount point>/<rest>: false"), "{bound}");
    assert!(bound.contains("edge: Some(Mount)"), "{bound}");
}

/// A file held open inside a child dataset is in use: one identity for the
/// guard, the walk and the process table, its device the dataset's
/// `f_fsid` folded (safety ADR, amendment of 2026-11-02, "Stable
/// identities").
#[test]
fn a_file_held_open_inside_a_dataset_is_in_use() {
    let Some(t) = common::mount("a_file_held_open_inside_a_dataset_is_in_use", Mount::ZfsTree) else {
        return;
    };
    let tree = TreeBuilder::new_in(&t.join("work"));
    common::mounts::held_open_is_in_use("a_file_held_open_inside_a_dataset_is_in_use", &tree.file("held", 4096));
}

/// An item inside a child dataset moves into `.Trash-$uid` at the
/// dataset's own root, the top of its mount: the same device, so the rename
/// works, and never the pool's top above it. The dry run said it would.
#[test]
fn a_dataset_item_moves_into_the_datasets_own_trash() {
    use ddc_core::platform::trash;
    use std::os::unix::fs::MetadataExt as _;
    let Some(top) = common::mount("a_dataset_item_moves_into_the_datasets_own_trash", Mount::ZfsChild) else {
        return;
    };
    let top = top.canonical().unwrap();
    let tree = TreeBuilder::new_in(&top);
    let file = tree.file("cache/blob", 4096);
    let id = ddc_core::platform::meta::lstat(&file).unwrap().id();
    let folder = ddc_core::platform::fsops::DirFd::open(file.parent().unwrap()).unwrap();
    let name = file.file_name().unwrap();
    let uid = std::fs::metadata(tree.root()).unwrap().uid();

    trash::would_trash(&folder, name, &file).unwrap();
    let place = trash::trash_item(&folder, name, &file, id).unwrap().expect("Linux tells the place");
    assert_eq!(place.trash, top.join(format!(".Trash-{uid}")));
    let landed = ddc_core::platform::meta::lstat(&place.trash.join("files").join(&place.name)).unwrap();
    assert_eq!(landed.id(), id);
    assert!(!file.exists());
    let info =
        std::fs::read_to_string(place.trash.join("info").join(format!("{}.trashinfo", place.name.to_string_lossy())))
            .unwrap();
    let rel = file.strip_prefix(&top).unwrap();
    assert!(info.contains(&format!("Path={}\n", rel.display())), "relative to the dataset's root: {info}");
    assert!(!top.parent().unwrap().join(format!(".Trash-{uid}")).exists(), "not the pool's top");
}

/// A project in a home and a scan whose finding lies inside a child
/// dataset of it.
fn project(home: &Path, rel: &str) -> (TreeBuilder, PathBuf) {
    let tree = TreeBuilder::new_in(&home.join(rel));
    let now = SystemTime::now();
    tree.write("p/package.json", "{}");
    tree.write("p/package-lock.json", "{}");
    tree.file("p/node_modules/dep/index.js", 20_000);
    for f in ["p/package.json", "p/package-lock.json"] {
        File::options()
            .write(true)
            .open(tree.path(f))
            .unwrap()
            .set_modified(now - Duration::from_secs(86_400 * 60))
            .unwrap();
    }
    let nm = tree.path("p/node_modules");
    (tree, nm)
}

/// A finding inside a child dataset is cleaned like any other: the guard
/// opens its parent beneath the dataset's own anchor, anchored beneath the
/// scan root's as the walk anchors it.
#[test]
fn a_finding_inside_a_dataset_is_deleted() {
    use common::fakes::{FakeCommands, FakeProcs, FakeTrash};
    use ddc_core::class::RiskClass;
    use ddc_core::env::Env;
    use ddc_core::exec::{self, Consent, ExecOptions, MemoryJournal, Outcome, Ports};
    use ddc_core::plan::{PlanOptions, build_plan};
    use ddc_core::report::{self, ScanOptions};
    use ddc_core::rules::Catalog;

    let Some(home) = common::mount("a_finding_inside_a_dataset_is_deleted", Mount::ZfsDelete) else {
        return;
    };
    let home = home.canonical().unwrap();
    let (tree, nm) = project(&home, "work");
    let data = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    let env = Env::with_paths(&home, data.path(), &data.path().join("logs"), now).unwrap();
    let scanned = report::scan(&env, &ScanOptions::default(), &Progress::default()).unwrap();
    let opts = PlanOptions { permanent: true, ..PlanOptions::default() };
    let plan = build_plan(&scanned, &Catalog::builtin(&home), &opts);
    assert!(plan.items.iter().any(|i| i.finding.path == nm), "{plan:#?}\n{}", trace(&home, "work"));

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
        exec::execute(&env, &Catalog::builtin(&home), &plan, &consent, ports, &ExecOptions::default()).unwrap();
    let item = report.items.iter().find(|i| i.path == nm).expect("planned");
    assert_eq!(item.outcome, Outcome::Deleted, "{report:#?}\n{}", trace(&home, "work"));
    assert!(!nm.exists());
    assert!(tree.path("p/package.json").exists());
}

/// A finding that is itself a dataset is refused by the guard on its own,
/// even from a plan that calls it complete: it is a mount root. Nothing is
/// removed.
#[test]
fn the_guard_refuses_a_finding_that_is_a_dataset_whatever_the_plan_says() {
    use common::fakes::{FakeCommands, FakeProcs, FakeTrash};
    use ddc_core::class::RiskClass;
    use ddc_core::env::Env;
    use ddc_core::exec::{self, Consent, ExecOptions, MemoryJournal, Outcome, Ports, SkipReason as Skip};
    use ddc_core::plan::{PlanOptions, build_plan};
    use ddc_core::report::{self, ScanOptions};
    use ddc_core::rules::Catalog;

    let Some(home) =
        common::mount("the_guard_refuses_a_finding_that_is_a_dataset_whatever_the_plan_says", Mount::ZfsGuard)
    else {
        return;
    };
    let home = home.canonical().unwrap();
    let now = SystemTime::now();
    let p = home.join("p");
    for f in ["package.json", "package-lock.json"] {
        std::fs::write(p.join(f), "{}").unwrap();
        File::options()
            .write(true)
            .open(p.join(f))
            .unwrap()
            .set_modified(now - Duration::from_secs(86_400 * 60))
            .unwrap();
    }
    let nm = p.join("node_modules");
    std::fs::create_dir_all(nm.join("dep")).unwrap();
    std::fs::write(nm.join("dep/index.js"), vec![0u8; 20_000]).unwrap();

    let data = tempfile::tempdir().unwrap();
    let env = Env::with_paths(&home, data.path(), &data.path().join("logs"), now).unwrap();
    let mut scanned = report::scan(&env, &ScanOptions::default(), &Progress::default()).unwrap();
    let finding = scanned.findings.iter_mut().find(|f| f.path == nm).expect("found");
    assert!(finding.incomplete, "the scan marks it\n{}", trace(&home, "p/node_modules"));
    finding.incomplete = false;
    let opts = PlanOptions { permanent: true, ..PlanOptions::default() };
    let plan = build_plan(&scanned, &Catalog::builtin(&home), &opts);
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
        exec::execute(&env, &Catalog::builtin(&home), &plan, &consent, ports, &ExecOptions::default()).unwrap();
    let item = report.items.iter().find(|i| i.path == nm).expect("planned");
    assert_eq!(item.outcome, Outcome::Skipped(Skip::Incomplete), "{report:#?}");
    assert!(nm.join("dep/index.js").exists(), "nothing removed");
}
