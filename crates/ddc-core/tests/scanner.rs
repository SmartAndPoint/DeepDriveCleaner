mod common;

use std::path::{Path, PathBuf};

use common::{TreeBuilder, alloc, reference_alloc};
use ddc_core::scan::{
    self, DirCtx, DirListing, NullVisitor, OwnerKind, OwnerResult, Post, Pre, Progress, ReadErrorKind, ReaderKind, Tag,
    Visitor, WalkOptions, WalkOutput,
};

const CLAIM_BY_NAME: Tag = 1;
const CLAIM_BY_PATH: Tag = 2;
const REGION: Tag = 3;

/// Claims directories named `claimme*` after reading them, claims or marks
/// as region the given exact paths before reading them.
#[derive(Default)]
struct TestVisitor {
    claim_paths: Vec<PathBuf>,
    region_paths: Vec<PathBuf>,
}

impl Visitor for TestVisitor {
    fn before_read(&self, dir: &DirCtx<'_>) -> Pre {
        if self.claim_paths.iter().any(|p| p == dir.path) {
            Pre::Claim(CLAIM_BY_PATH)
        } else if self.region_paths.iter().any(|p| p == dir.path) {
            Pre::Region(REGION)
        } else {
            Pre::Continue
        }
    }

    fn after_read(&self, dir: &DirCtx<'_>, _listing: &DirListing) -> Post {
        if dir.name.to_str().is_some_and(|n| n.starts_with("claimme")) {
            Post::Claim(CLAIM_BY_NAME)
        } else {
            Post::Descend
        }
    }
}

fn opts(reader: ReaderKind) -> WalkOptions {
    WalkOptions { threads: Some(4), reader, ..WalkOptions::default() }
}

fn walk_with(root: &Path, visitor: &dyn Visitor, reader: ReaderKind) -> WalkOutput {
    scan::walk(&[root.to_path_buf()], visitor, &opts(reader), &Progress::default()).expect("walk")
}

fn walk(root: &Path, visitor: &dyn Visitor) -> WalkOutput {
    walk_with(root, visitor, ReaderKind::Bulk)
}

fn owner<'a>(out: &'a WalkOutput, path: &Path) -> &'a OwnerResult {
    out.owners.iter().find(|o| o.path == path).unwrap_or_else(|| panic!("no owner {path:?} in {:#?}", out.owners))
}

#[test]
fn allocated_size_matches_lstat_blocks() {
    let t = TreeBuilder::new();
    t.file("empty", 0);
    t.file("one-byte", 1);
    t.file("a/page", 4096);
    t.file("a/b/medium", 100_000);
    t.file("a/b/c/large", 1 << 20);
    let sparse = t.sparse("a/sparse", 10 << 20);
    assert_eq!(alloc(&sparse), 0, "test precondition: sparse file allocates nothing");

    let out = walk(t.root(), &NullVisitor);

    assert_eq!(out.total_bytes, reference_alloc(t.root()));
    assert_eq!(out.roots[0].files, 6);
    assert_eq!(out.roots[0].dirs, 4);
    assert_eq!(out.errors_total, 0);
}

#[test]
fn symlinks_are_not_followed() {
    let outside = TreeBuilder::new();
    outside.file("big", 1 << 20);
    let t = TreeBuilder::new();
    t.file("own", 10_000);
    t.symlink(outside.root(), "to-outside");
    t.symlink(".", "loop");
    t.symlink("missing", "dangling");

    let out = walk(t.root(), &NullVisitor);

    let own = alloc(&t.path("own"));
    let links: u64 = ["to-outside", "loop", "dangling"].iter().map(|l| alloc(&t.path(l))).sum();
    assert_eq!(out.total_bytes, own + links);
    assert_eq!(out.roots[0].dirs, 1);
}

#[test]
fn unreadable_directory_is_reported_and_siblings_are_counted() {
    let t = TreeBuilder::new();
    t.file("locked/secret", 50_000);
    let sibling = t.file("open/data", 50_000);
    let locked = t.lock("locked");
    if std::fs::read_dir(&locked).is_ok() {
        eprintln!("SKIPPED: running as root, permissions are not enforced");
        return;
    }

    let out = walk(t.root(), &NullVisitor);

    assert_eq!(out.errors_total, 1);
    assert_eq!(out.errors[0].path, locked);
    assert_eq!(out.errors[0].error.kind, ReadErrorKind::PermissionDenied);
    assert_eq!(out.total_bytes, alloc(&sibling));
}

#[test]
fn claimed_subtree_is_attributed_to_its_owner() {
    let t = TreeBuilder::new();
    t.file("loose", 30_000);
    t.file("proj/claimme/a", 40_000);
    t.file("proj/claimme/deep/b", 50_000);
    t.file("proj/claimme/claimme-nested/c", 60_000);
    t.file("known/x", 70_000);
    let claim = t.path("proj/claimme");
    let known = t.path("known");
    let v = TestVisitor { claim_paths: vec![known.clone()], ..Default::default() };

    let out = walk(t.root(), &v);

    assert_eq!(out.owners.len(), 2, "nested match inside a claim is not a separate finding: {:#?}", out.owners);
    let c = owner(&out, &claim);
    assert_eq!((c.tag, c.kind), (CLAIM_BY_NAME, OwnerKind::Claim));
    assert_eq!(c.bytes, reference_alloc(&claim));
    assert_eq!((c.files, c.dirs), (3, 3));
    let k = owner(&out, &known);
    assert_eq!((k.tag, k.bytes), (CLAIM_BY_PATH, reference_alloc(&known)));
    assert_eq!(out.unowned_bytes, alloc(&t.path("loose")));
    assert_eq!(out.total_bytes, reference_alloc(t.root()));
}

#[test]
fn region_keeps_rules_working_inside() {
    let t = TreeBuilder::new();
    t.file("downloads/movie", 80_000);
    t.file("downloads/repo/claimme/dep", 20_000);
    let region = t.path("downloads");
    let v = TestVisitor { region_paths: vec![region.clone()], ..Default::default() };

    let out = walk(t.root(), &v);

    let r = owner(&out, &region);
    assert_eq!(r.kind, OwnerKind::Region);
    assert_eq!(r.bytes, alloc(&t.path("downloads/movie")));
    assert_eq!(r.private_bytes, None, "regions are not findings; private size is not measured");
    let c = owner(&out, &t.path("downloads/repo/claimme"));
    assert_eq!(c.bytes, alloc(&t.path("downloads/repo/claimme/dep")));
}

#[test]
fn hard_links_inside_one_finding_are_exclusive() {
    let t = TreeBuilder::new();
    let a = t.file("claimme/a", 100_000);
    t.hardlink(&a, "claimme/sub/b");

    let out = walk(t.root(), &TestVisitor::default());

    let c = owner(&out, &t.path("claimme"));
    assert_eq!((c.bytes, c.shared_bytes), (alloc(&a), 0));
    assert_eq!(out.total_bytes, alloc(&a), "an inode counts once");
}

#[test]
fn hard_links_across_findings_are_shared_by_both() {
    let t = TreeBuilder::new();
    let only1 = t.file("claimme1/only", 10_000);
    let a = t.file("claimme1/a", 100_000);
    t.hardlink(&a, "claimme2/b");

    let out = walk(t.root(), &TestVisitor::default());

    let c1 = owner(&out, &t.path("claimme1"));
    let c2 = owner(&out, &t.path("claimme2"));
    assert_eq!((c1.bytes, c1.shared_bytes), (alloc(&only1), alloc(&a)));
    assert_eq!((c2.bytes, c2.shared_bytes), (0, alloc(&a)));
    assert!(c1.private_bytes.unwrap() <= alloc(&only1), "shared inodes are not in the private size");
    assert_eq!(c2.private_bytes, Some(0));
    assert_eq!(out.total_bytes, alloc(&only1) + alloc(&a));
}

#[test]
fn hard_link_to_a_file_outside_the_scan_is_shared() {
    let outside = TreeBuilder::new();
    let src = outside.file("src", 100_000);
    let t = TreeBuilder::new();
    if std::fs::hard_link(&src, t.path("probe")).is_err() {
        eprintln!("SKIPPED: temp dirs are on different volumes");
        return;
    }
    std::fs::rename(t.path("probe"), {
        t.dir("claimme");
        t.path("claimme/link")
    })
    .unwrap();

    let out = walk(t.root(), &TestVisitor::default());

    let c = owner(&out, &t.path("claimme"));
    assert_eq!((c.bytes, c.shared_bytes), (0, alloc(&src)));
}

#[test]
fn apfs_clone_has_near_zero_private_size() {
    let t = TreeBuilder::new();
    let src = t.file("src", 1 << 20);
    let clone = t.clone_file(&src, "claimme-clone/copy");
    let plain = t.file("claimme-plain/data", 1 << 20);

    let out = walk(t.root(), &TestVisitor::default());

    let c = owner(&out, &t.path("claimme-clone"));
    assert_eq!(alloc(&clone), alloc(&src), "test precondition: clone allocates like the original");
    assert_eq!(c.bytes, alloc(&clone), "a clone occupies its full size in du terms");
    assert!(c.private_bytes.unwrap() < 64 * 1024, "clone frees ~nothing: {:?}", c.private_bytes);
    let p = owner(&out, &t.path("claimme-plain"));
    assert_eq!(p.private_bytes, Some(alloc(&plain)));
}

#[test]
fn walk_is_deterministic() {
    let t = TreeBuilder::new();
    for i in 0..20 {
        let f = t.file(&format!("claimme{}/f", i % 3), 10_000 + i * 1000);
        t.hardlink(&f, &format!("claimme{}/link{i}", (i + 1) % 3));
        t.file(&format!("loose/{i}"), 5_000);
    }

    let first = walk(t.root(), &TestVisitor::default());
    for _ in 0..5 {
        let again = walk(t.root(), &TestVisitor::default());
        assert_eq!(again.owners, first.owners);
        assert_eq!((again.total_bytes, again.unowned_bytes), (first.total_bytes, first.unowned_bytes));
        assert_eq!(again.roots, first.roots);
    }
}

#[test]
fn bulk_and_std_readers_agree() {
    let t = TreeBuilder::new();
    for i in 0..50 {
        t.file(&format!("d{}/e{}/f{i}", i % 5, i % 7), 1000 * i);
    }
    let a = t.file("x/a", 70_000);
    t.hardlink(&a, "y/b");
    t.symlink("x", "link");
    t.sparse("x/sparse", 1 << 20);

    let bulk = walk_with(t.root(), &TestVisitor::default(), ReaderKind::Bulk);
    let std = walk_with(t.root(), &TestVisitor::default(), ReaderKind::Std);

    assert_eq!(bulk.roots, std.roots);
    assert_eq!(bulk.total_bytes, reference_alloc(t.root()));
}

#[test]
fn nested_roots_are_merged() {
    let t = TreeBuilder::new();
    t.file("a/b/f", 20_000);

    let out = scan::walk(
        &[t.path("a/b"), t.path("a"), t.path("a/../a")],
        &NullVisitor,
        &opts(ReaderKind::Bulk),
        &Progress::default(),
    )
    .unwrap();

    assert_eq!(out.roots.len(), 1);
    assert_eq!(out.roots[0].path, t.path("a"));
    assert_eq!(out.total_bytes, reference_alloc(&t.path("a")));
}

#[test]
fn dataless_materialization_can_be_disabled() {
    ddc_core::sys::iopolicy::disable_dataless_materialization().unwrap();
    assert!(ddc_core::sys::iopolicy::dataless_materialization_disabled().unwrap());
}

#[test]
fn file_root_is_counted() {
    let t = TreeBuilder::new();
    let f = t.file("single", 12_345);
    let out = walk(&f, &NullVisitor);
    assert_eq!((out.total_bytes, out.roots[0].files), (alloc(&f), 1));
}

#[test]
fn private_size_is_unknown_when_not_returned() {
    let t = TreeBuilder::new();
    t.file("claimme/a", 50_000);

    let std = walk_with(t.root(), &TestVisitor::default(), ReaderKind::Std);
    let bulk = walk_with(t.root(), &TestVisitor::default(), ReaderKind::Bulk);

    let path = t.path("claimme");
    assert_eq!(owner(&std, &path).private_bytes, None, "std::fs cannot measure private size");
    assert_eq!(owner(&bulk, &path).private_bytes, Some(alloc(&t.path("claimme/a"))));
}

#[test]
fn bulk_reader_matches_lstat_per_entry_across_batches() {
    use std::collections::HashMap;

    use ddc_core::scan::{DirReader, EntryKind, StdReader};
    use ddc_core::sys::bulk::BulkReader;

    let t = TreeBuilder::new();
    let dir = t.dir("big");
    // The 128 KiB attribute buffer holds ~1,500 records: force several batches.
    for i in 0..3000 {
        t.file(&format!("big/f{i:04}"), (i % 7) * 1000);
    }
    t.file(&format!("big/{}", "x".repeat(255)), 10);
    for n in ["файл-кириллица", "emoji-😀-name", "e\u{301}-nfd", "tab\tname", "new\nline"] {
        t.file(&format!("big/{n}"), 100);
    }
    let a = t.file("big/hl-src", 5000);
    t.hardlink(&a, "big/hl-dst");
    t.symlink("f0001", "big/link");
    t.dir("big/subdir");
    std::fs::write(t.path("big/f0002/..namedfork/rsrc"), vec![1u8; 300_000]).unwrap();
    let fifo = std::process::Command::new("/usr/bin/mkfifo").arg(t.path("big/fifo")).status().unwrap();
    assert!(fifo.success());

    let reference = StdReader.read(&dir, None, false).unwrap();
    let by_name: HashMap<_, _> = reference.entries.iter().map(|e| (e.name.clone(), e)).collect();
    for want_private in [false, true] {
        let bulk = BulkReader.read(&dir, None, want_private).unwrap();
        assert_eq!((bulk.dev, bulk.ino), (reference.dev, reference.ino));
        assert_eq!(bulk.entries.len(), reference.entries.len());
        for b in &bulk.entries {
            let r = by_name.get(&b.name).unwrap_or_else(|| panic!("unexpected entry {:?}", b.name));
            assert_eq!(
                (b.kind, b.dev, b.ino, b.flags, b.alloc),
                (r.kind, r.dev, r.ino, r.flags, r.alloc),
                "{:?}",
                b.name
            );
            if b.kind != EntryKind::Dir {
                assert_eq!(b.nlink, r.nlink, "{:?}", b.name);
            }
            match (want_private, b.kind) {
                (true, EntryKind::File) => {
                    assert!(b.private.is_some_and(|p| p <= b.alloc), "{:?}: {:?}", b.name, b.private)
                }
                (false, _) => assert_eq!(b.private, None),
                _ => {}
            }
        }
    }
    let fork = reference.entries.iter().find(|e| e.name == "f0002").unwrap();
    assert!(fork.alloc >= 300_000, "resource fork is part of the allocated size: {}", fork.alloc);
}
