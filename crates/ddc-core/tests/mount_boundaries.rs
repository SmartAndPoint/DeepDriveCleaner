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
