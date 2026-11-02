//! The walk stays on one mount (safety ADR, amendment of 2026-11-01; story
//! "Linux mount boundaries: automounts, bind mounts and the Trash across
//! filesystems", DDC-010), on the real mounts of the Gate's linux-mounts job.
//! Elsewhere every test here skips, in the environment group of
//! `.config/native-skips/linux.txt`.
#![cfg(target_os = "linux")]

mod common;

use std::path::Path;

use common::{Mount, TreeBuilder};
use ddc_core::scan::{self, DirCtx, DirListing, NullVisitor, Post, Progress, SkipReason, Tag, Visitor, WalkOptions};

/// Claims every folder named `node_modules`, as the catalog does.
struct NodeModules;

impl Visitor for NodeModules {
    fn after_read(&self, dir: &DirCtx<'_>, _listing: &DirListing) -> Post {
        if dir.name == "node_modules" { Post::Claim(1 as Tag) } else { Post::Descend }
    }
}

fn walk(root: &Path, visitor: &dyn Visitor) -> scan::WalkOutput {
    scan::walk(&[root.to_path_buf()], visitor, &WalkOptions::default(), &Progress::default()).expect("walk")
}

fn skipped(out: &scan::WalkOutput, path: &Path) -> Option<SkipReason> {
    out.skipped.iter().find(|s| s.path == path).map(|s| s.reason)
}

/// A bind mount of the same filesystem keeps its device, so the device
/// check alone entered it, counted it and let a rule claim inside it.
#[test]
fn a_bind_mount_inside_a_root_is_skipped_never_counted_and_never_claimed() {
    let Some(root) =
        common::mount("a_bind_mount_inside_a_root_is_skipped_never_counted_and_never_claimed", Mount::Bind)
    else {
        return;
    };
    let inner = root.join("inner");
    let tree = TreeBuilder::new_in(&inner);
    tree.file("project/node_modules/dep/index.js", 64 * 1024);

    let out = walk(&root, &NodeModules);
    assert_eq!(skipped(&out, &inner), Some(SkipReason::OtherFilesystem), "{:#?}", out.skipped);
    assert!(!out.owners.iter().any(|o| o.path.starts_with(&inner)), "nothing inside the bind mount is a finding");
    let bytes = out.roots.iter().find(|r| r.path == root).map(|r| r.bytes).unwrap();
    assert!(bytes < 64 * 1024, "the bind mount's file is not counted: {bytes}");
}

/// The trigger of an autofs direct map is an autofs mount root. A scan of
/// its folder reports it as an automount point and leaves it unmounted:
/// mountinfo shows the autofs mount alone at the trigger before and after.
/// The only test that touches the trigger.
#[test]
fn an_autofs_direct_map_is_reported_and_never_triggered_by_a_scan() {
    let Some(trigger) = common::mount("an_autofs_direct_map_is_reported_and_never_triggered_by_a_scan", Mount::Autofs)
    else {
        return;
    };
    let before = common::mounts::mounted_at(&trigger);
    assert_eq!(before, vec!["autofs".to_owned()], "untriggered before the scan");

    let out = walk(trigger.parent().unwrap(), &NullVisitor);
    assert_eq!(skipped(&out, &trigger), Some(SkipReason::Automount), "{:#?}", out.skipped);
    assert_eq!(common::mounts::mounted_at(&trigger), before, "the scan mounted nothing");
}

/// The open beneath the root refuses the bind mount itself, with `openat2`
/// (`EXDEV`) and with the by-path fallback (its mount id), so a folder
/// swapped for a bind mount after its listing is never entered either.
#[test]
fn a_bind_mount_is_refused_beneath_its_root_with_openat2_and_without() {
    use ddc_core::platform::bulk;
    use ddc_core::scan::{ReadErrorKind, Times};
    let Some(root) = common::mount("a_bind_mount_is_refused_beneath_its_root_with_openat2_and_without", Mount::Bind)
    else {
        return;
    };
    for anchor in [bulk::anchor(&root).unwrap(), bulk::anchor_by_path(&root).unwrap()] {
        let err = bulk::list_in(&anchor, &root.join("inner"), None, false, Times::None).unwrap_err();
        assert_eq!(err.kind, ReadErrorKind::OtherFilesystem, "{anchor:?}");
    }
}

/// The guard's stat of a child tells a bind mount of the same filesystem,
/// which its `dev` does not (safety ADR §5, amendment of 2026-11-01).
#[test]
fn the_guards_stat_tells_a_bind_mount_as_a_mount_root() {
    use ddc_core::platform::bulk;
    use ddc_core::platform::fsops::DirFd;
    let Some(root) = common::mount("the_guards_stat_tells_a_bind_mount_as_a_mount_root", Mount::Bind) else {
        return;
    };
    let dir = DirFd::open(&root).unwrap();
    let inner = dir.stat_child(std::ffi::OsStr::new("inner")).unwrap();
    assert!(inner.mount_root, "the bind mount");
    assert_eq!(inner.dev, dir.identity().unwrap().0, "the same device");
    for anchor in [bulk::anchor(&root).unwrap(), bulk::anchor_by_path(&root).unwrap()] {
        assert!(DirFd::open_beneath(&anchor, &root).is_ok(), "{anchor:?}");
        assert!(DirFd::open_beneath(&anchor, &root.join("inner")).is_err(), "{anchor:?}");
    }
}

/// A plan whose finding now lies behind a bind mount, with the same inode
/// on both sides, is refused before anything moves: its parent opens
/// beneath the scan root, and the bind mount is not crossed. Opened by path,
/// as before, it passed every identity check.
#[test]
fn a_stale_plan_into_a_bind_mount_is_refused_and_nothing_moves() {
    use std::fs::File;
    use std::time::{Duration, SystemTime};

    use common::fakes::{FakeCommands, FakeProcs, FakeTrash};
    use ddc_core::class::RiskClass;
    use ddc_core::env::Env;
    use ddc_core::exec::{self, Consent, ExecOptions, MemoryJournal, Outcome, Ports, SkipReason as Skip};
    use ddc_core::plan::{PlanOptions, build_plan};
    use ddc_core::report::{self, ScanOptions};
    use ddc_core::rules::Catalog;

    let Some(root) = common::mount("a_stale_plan_into_a_bind_mount_is_refused_and_nothing_moves", Mount::Bind) else {
        return;
    };
    // The tree through the bind mount, and the same folder at its source.
    let tree = TreeBuilder::new_in(&root.join("inner"));
    let source = root.parent().unwrap().join("bind-source").join(tree.root().file_name().unwrap());
    assert!(source.is_dir(), "{source:?}");
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
    let data = tempfile::tempdir().unwrap();
    let env = |home: &Path| Env::with_paths(home, data.path(), &data.path().join("logs"), now).unwrap();

    // Planned at the source, then pointed through the bind mount.
    let scanned = report::scan(&env(&source), &ScanOptions::default(), &Progress::default()).unwrap();
    let mut plan = build_plan(&scanned, &Catalog::builtin(&source), &PlanOptions::default());
    assert!(!plan.items.is_empty(), "the project's node_modules is planned");
    plan.roots = vec![root.clone()];
    for item in &mut plan.items {
        item.finding.path = tree.root().join(item.finding.path.strip_prefix(&source).unwrap());
    }
    // And one at the bind mount itself, with the identity of the folder
    // it shows: the parent opens, and the entry is a mount root.
    let mut at_mount = plan.items[0].clone();
    let id = |p: &Path| {
        let m = std::fs::symlink_metadata(p).unwrap();
        (std::os::unix::fs::MetadataExt::dev(&m), std::os::unix::fs::MetadataExt::ino(&m))
    };
    at_mount.finding.path = root.join("inner");
    (at_mount.finding.dev, at_mount.finding.ino) = id(&root.parent().unwrap().join("bind-source"));
    (at_mount.finding.parent_dev, at_mount.finding.parent_ino) = (Some(id(&root).0), Some(id(&root).1));
    plan.items.push(at_mount);

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
        exec::execute(&env(&root), &Catalog::builtin(&root), &plan, &consent, ports, &ExecOptions::default()).unwrap();
    let nm = report.items.iter().find(|i| i.path == tree.path("p/node_modules")).expect("planned");
    assert_eq!(nm.outcome, Outcome::Skipped(Skip::ParentChanged));
    let at_mount = report.items.iter().find(|i| i.path == root.join("inner")).expect("planned");
    assert_eq!(at_mount.outcome, Outcome::Skipped(Skip::Incomplete));
    assert!(tree.path("p/node_modules/dep/index.js").exists(), "nothing moved");
}
