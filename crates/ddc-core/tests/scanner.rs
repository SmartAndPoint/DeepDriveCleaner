mod common;

use std::path::{Path, PathBuf};

use common::{OLD, TreeBuilder, alloc, folders, reference_alloc};
use ddc_core::platform::Capability;
use ddc_core::scan::{
    self, Boundary, DirCtx, DirListing, DirReader, EntryKind, EntryResult, NullVisitor, OwnerKind, OwnerResult, Post,
    Pre, Progress, ReadError, ReadErrorKind, ReaderKind, StdReader, Tag, Times, Visitor, WalkOptions, WalkOutput,
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
    /// Claims or regions whose direct children are measured one by one.
    entry_paths: Vec<PathBuf>,
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

    fn lists_entries(&self, dir: &DirCtx<'_>, _tag: Tag) -> bool {
        self.entry_paths.iter().any(|p| p == dir.path)
    }
}

fn entries<'a>(out: &'a WalkOutput, owner: &Path) -> &'a [EntryResult] {
    &out.entries.iter().find(|e| e.owner == owner).unwrap_or_else(|| panic!("no entries of {owner:?}")).entries
}

fn entry<'a>(out: &'a WalkOutput, owner: &Path, name: &str) -> &'a EntryResult {
    entries(out, owner).iter().find(|e| e.name == name).unwrap_or_else(|| panic!("no entry {name}"))
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
    if common::unsupported("allocated_size_matches_lstat_blocks") {
        return;
    }
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
    if common::unsupported("symlinks_are_not_followed") {
        return;
    }
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
    assert_eq!(out.total_bytes, own + links + folders(&[t.root()]));
    assert_eq!(out.roots[0].dirs, 1);
}

#[test]
fn unreadable_directory_is_reported_and_siblings_are_counted() {
    if common::unsupported("unreadable_directory_is_reported_and_siblings_are_counted") {
        return;
    }
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
    // The locked folder is never read; its own blocks count, as du counts them.
    assert_eq!(out.total_bytes, alloc(&sibling) + folders(&[t.root(), &t.path("open"), &locked]));
}

#[test]
fn claimed_subtree_is_attributed_to_its_owner() {
    if common::unsupported("claimed_subtree_is_attributed_to_its_owner") {
        return;
    }
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
    assert_eq!(out.unowned_bytes, alloc(&t.path("loose")) + folders(&[t.root(), &t.path("proj")]));
    assert_eq!(out.total_bytes, reference_alloc(t.root()));
}

#[test]
fn region_keeps_rules_working_inside() {
    if common::unsupported("region_keeps_rules_working_inside") {
        return;
    }
    let t = TreeBuilder::new();
    t.file("downloads/movie", 80_000);
    t.file("downloads/repo/claimme/dep", 20_000);
    let region = t.path("downloads");
    let v = TestVisitor { region_paths: vec![region.clone()], ..Default::default() };

    let out = walk(t.root(), &v);

    let r = owner(&out, &region);
    assert_eq!(r.kind, OwnerKind::Region);
    assert_eq!(r.bytes, alloc(&t.path("downloads/movie")) + folders(&[&region, &t.path("downloads/repo")]));
    assert_eq!(r.private_bytes, None, "regions are not findings; private size is not measured");
    let c = owner(&out, &t.path("downloads/repo/claimme"));
    assert_eq!(c.bytes, reference_alloc(&t.path("downloads/repo/claimme")));
}

#[test]
fn hard_links_inside_one_finding_are_exclusive() {
    if common::unsupported("hard_links_inside_one_finding_are_exclusive") {
        return;
    }
    let t = TreeBuilder::new();
    let a = t.file("claimme/a", 100_000);
    t.hardlink(&a, "claimme/sub/b");

    let out = walk(t.root(), &TestVisitor::default());

    let c = owner(&out, &t.path("claimme"));
    let dirs = folders(&[t.path("claimme"), t.path("claimme/sub")]);
    assert_eq!((c.bytes, c.shared_bytes), (alloc(&a) + dirs, 0));
    assert_eq!(out.total_bytes, alloc(&a) + dirs + folders(&[t.root()]), "an inode counts once");
}

#[test]
fn hard_links_across_findings_are_shared_by_both() {
    if common::unsupported("hard_links_across_findings_are_shared_by_both") {
        return;
    }
    let t = TreeBuilder::new();
    let only1 = t.file("claimme1/only", 10_000);
    let a = t.file("claimme1/a", 100_000);
    t.hardlink(&a, "claimme2/b");

    let out = walk(t.root(), &TestVisitor::default());

    let c1 = owner(&out, &t.path("claimme1"));
    let c2 = owner(&out, &t.path("claimme2"));
    let (d1, d2) = (folders(&[t.path("claimme1")]), folders(&[t.path("claimme2")]));
    assert_eq!((c1.bytes, c1.shared_bytes), (alloc(&only1) + d1, alloc(&a)));
    assert_eq!((c2.bytes, c2.shared_bytes), (d2, alloc(&a)));
    if ddc_core::platform::CAPABILITIES.contains(&ddc_core::platform::Capability::PrivateSize) {
        assert!(c1.private_bytes.unwrap() <= alloc(&only1), "shared inodes are not in the private size");
        assert_eq!(c2.private_bytes, Some(0));
    } else {
        assert_eq!((c1.private_bytes, c2.private_bytes), (None, None), "no private size measured");
    }
    assert_eq!(out.total_bytes, alloc(&only1) + alloc(&a) + d1 + d2 + folders(&[t.root()]));
}

#[test]
fn hard_link_to_a_file_outside_the_scan_is_shared() {
    if common::unsupported("hard_link_to_a_file_outside_the_scan_is_shared") {
        return;
    }
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
    assert_eq!((c.bytes, c.shared_bytes), (folders(&[t.path("claimme")]), alloc(&src)));
}

#[test]
fn apfs_clone_has_near_zero_private_size() {
    if common::unsupported("apfs_clone_has_near_zero_private_size")
        || common::lacks("apfs_clone_has_near_zero_private_size", Capability::PrivateSize)
    {
        return;
    }
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
    if common::unsupported("walk_is_deterministic") {
        return;
    }
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
    if common::unsupported("bulk_and_std_readers_agree") {
        return;
    }
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
    if common::unsupported("nested_roots_are_merged") {
        return;
    }
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

// macOS only: dataless (evicted cloud) files exist only there.
#[cfg(target_os = "macos")]
#[test]
fn dataless_materialization_can_be_disabled() {
    ddc_core::platform::iopolicy::disable_dataless_materialization().unwrap();
    assert!(ddc_core::platform::iopolicy::dataless_materialization_disabled().unwrap());
}

#[test]
fn file_root_is_counted() {
    if common::unsupported("file_root_is_counted") {
        return;
    }
    let t = TreeBuilder::new();
    let f = t.file("single", 12_345);
    let out = walk(&f, &NullVisitor);
    assert_eq!((out.total_bytes, out.roots[0].files), (alloc(&f), 1));
}

#[test]
fn private_size_is_unknown_when_not_returned() {
    if common::unsupported("private_size_is_unknown_when_not_returned")
        || common::lacks("private_size_is_unknown_when_not_returned", Capability::PrivateSize)
    {
        return;
    }
    let t = TreeBuilder::new();
    t.file("claimme/a", 50_000);

    let std = walk_with(t.root(), &TestVisitor::default(), ReaderKind::Std);
    let bulk = walk_with(t.root(), &TestVisitor::default(), ReaderKind::Bulk);

    let path = t.path("claimme");
    assert_eq!(owner(&std, &path).private_bytes, None, "std::fs cannot measure private size");
    assert_eq!(owner(&bulk, &path).private_bytes, Some(alloc(&t.path("claimme/a"))));
}

/// Without the `private-size` capability nothing asks for a private size,
/// even everywhere, so no claim reports one (ADR "Platform seam: one
/// platform module per OS with declared capabilities", §3).
#[test]
fn without_the_private_size_capability_no_claim_reports_one() {
    if common::unsupported("without_the_private_size_capability_no_claim_reports_one") {
        return;
    }
    let t = TreeBuilder::new();
    t.file("claimme/a", 50_000);
    let o = WalkOptions { private_size: false, private_everywhere: true, ..opts(ReaderKind::Bulk) };
    let out = scan::walk(&[t.root().to_path_buf()], &TestVisitor::default(), &o, &Progress::default()).expect("walk");
    assert_eq!(owner(&out, &t.path("claimme")).private_bytes, None);
}

#[test]
fn bulk_reader_matches_lstat_per_entry_across_batches() {
    // The getattrlistbulk reader, with its forks, private sizes and added
    // times; `platform/linux/bulk.rs` tests the Linux reader against std.
    if common::unsupported("bulk_reader_matches_lstat_per_entry_across_batches")
        || common::lacks("bulk_reader_matches_lstat_per_entry_across_batches", Capability::PrivateSize)
    {
        return;
    }
    use std::collections::HashMap;

    use ddc_core::scan::SystemReader as BulkReader;
    use ddc_core::scan::{DirReader, EntryKind, StdReader, Times};

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

    let reference = StdReader.read(&dir, None, false, Times::None).unwrap();
    let by_name: HashMap<_, _> = reference.entries.iter().map(|e| (e.name.clone(), e)).collect();
    for want_private in [false, true] {
        let bulk = BulkReader.read(&dir, None, want_private, Times::None).unwrap();
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

    // Times only when asked for, the added time only with it, and the rest of
    // each record still in place: the modification time sits between the
    // object type and the flags, the added time after the file id.
    let plain = BulkReader.read(&dir, None, false, Times::None).unwrap();
    assert!(plain.entries.iter().all(|e| e.modified.is_none() && e.added.is_none()));
    let lstat = StdReader.read(&dir, None, false, Times::Modified).unwrap();
    let lstat_times: HashMap<_, _> = lstat.entries.into_iter().map(|e| (e.name, e.modified)).collect();
    let own = ddc_core::platform::meta::mtime(&std::fs::symlink_metadata(&dir).unwrap());
    assert_eq!((plain.modified, lstat.modified), (Some(own), Some(own)), "the directory's own time, always");
    for times in [Times::Modified, Times::ModifiedAndAdded] {
        for want_private in [false, true] {
            let timed = BulkReader.read(&dir, None, want_private, times).unwrap();
            assert_eq!(timed.entries.len(), reference.entries.len());
            assert_eq!(timed.modified, Some(own));
            for b in &timed.entries {
                let r = by_name[&b.name];
                assert_eq!(
                    (b.kind, b.dev, b.ino, b.flags, b.alloc),
                    (r.kind, r.dev, r.ino, r.flags, r.alloc),
                    "{:?} with {times:?}",
                    b.name
                );
                if b.kind != EntryKind::Dir {
                    assert_eq!(b.nlink, r.nlink, "{:?} with {times:?}", b.name);
                }
                assert!(b.modified.is_some() && b.modified == lstat_times[&b.name], "{:?}: {:?}", b.name, b.modified);
                assert_eq!(b.added.is_some(), times == Times::ModifiedAndAdded, "{:?}: {:?}", b.name, b.added);
                if want_private && b.kind == EntryKind::File {
                    assert!(b.private.is_some(), "{:?}", b.name);
                }
            }
        }
    }
}

#[test]
fn listed_entries_are_measured_one_by_one_with_claims_inside_folded_back() {
    if common::unsupported("listed_entries_are_measured_one_by_one_with_claims_inside_folded_back") {
        return;
    }
    let t = TreeBuilder::new();
    let region = t.dir("Downloads");
    let pics = t.dir("Pictures");
    t.file("Downloads/a.zip", 100_000);
    t.file("Downloads/b/x", 50_000);
    t.file("Downloads/b/y/z", 30_000);
    t.file("Downloads/repo/src/main.rs", 4_000);
    t.file("Downloads/repo/claimme-deps/lib/big", 200_000);
    t.symlink("a.zip", "Downloads/link");
    t.file("Pictures/trip/one.jpg", 70_000);
    t.file("Pictures/loose.png", 20_000);
    t.file("elsewhere/not-listed", 10_000);
    let visitor = TestVisitor {
        region_paths: vec![region.clone()],
        claim_paths: vec![pics.clone()],
        entry_paths: vec![region.clone(), pics.clone()],
    };

    let out = walk(t.root(), &visitor);

    let names: Vec<_> = entries(&out, &region).iter().map(|e| e.name.to_string_lossy().into_owned()).collect();
    assert_eq!(names, ["a.zip", "b", "link", "repo"], "sorted by name, every kind");
    assert_eq!(entry(&out, &region, "a.zip").bytes, alloc(&t.path("Downloads/a.zip")));
    assert_eq!(entry(&out, &region, "b").bytes, reference_alloc(&t.path("Downloads/b")));
    assert_eq!(entry(&out, &region, "link").kind, EntryKind::Symlink);
    // The claim inside keeps its own owner and still counts for the entry that holds it.
    let repo = entry(&out, &region, "repo");
    assert_eq!(repo.bytes, reference_alloc(&t.path("Downloads/repo")));
    let deps = owner(&out, &t.path("Downloads/repo/claimme-deps"));
    assert_eq!(deps.bytes, reference_alloc(&t.path("Downloads/repo/claimme-deps")));
    assert!(owner(&out, &region).bytes < repo.bytes + deps.bytes, "the region's remainder leaves the claim out");
    for e in entries(&out, &region).iter().chain(entries(&out, &pics)) {
        assert!(e.modified.is_some() && e.added.is_some(), "{:?}", e.name);
        assert_eq!((e.errors, e.skipped, e.other_fs, e.error), (0, 0, 0, None), "{:?}", e.name);
    }
    // A claimed folder lists its children too.
    assert_eq!(entry(&out, &pics, "trip").bytes, reference_alloc(&t.path("Pictures/trip")));
    assert_eq!(entry(&out, &pics, "loose.png").bytes, alloc(&t.path("Pictures/loose.png")));
    assert_eq!(out.entries.len(), 2, "only the owners that list entries");
}

#[test]
fn a_hard_link_counts_for_an_entry_only_when_every_link_is_inside_it() {
    if common::unsupported("a_hard_link_counts_for_an_entry_only_when_every_link_is_inside_it") {
        return;
    }
    let t = TreeBuilder::new();
    let region = t.dir("Downloads");
    let inside = t.file("Downloads/both/one", 40_000);
    t.hardlink(&inside, "Downloads/both/sub/two");
    let split = t.file("Downloads/left/one", 60_000);
    t.hardlink(&split, "Downloads/right/two");
    let visitor =
        TestVisitor { region_paths: vec![region.clone()], entry_paths: vec![region.clone()], ..TestVisitor::default() };

    let out = walk(t.root(), &visitor);

    let dir = |rel: &str| folders(&[t.path(rel)]);
    assert_eq!(entry(&out, &region, "both").bytes, alloc(&inside) + dir("Downloads/both") + dir("Downloads/both/sub"));
    assert_eq!(entry(&out, &region, "left").bytes, dir("Downloads/left"));
    assert_eq!(entry(&out, &region, "right").bytes, dir("Downloads/right"));
}

#[test]
fn listed_entries_are_deterministic() {
    if common::unsupported("listed_entries_are_deterministic") {
        return;
    }
    let t = TreeBuilder::new();
    let region = t.dir("Downloads");
    for i in 0..40 {
        t.file(&format!("Downloads/d{i}/f"), 1_000 * i);
        t.file(&format!("Downloads/f{i}"), 500 * i);
    }
    let visitor =
        TestVisitor { region_paths: vec![region.clone()], entry_paths: vec![region.clone()], ..TestVisitor::default() };
    assert_eq!(walk(t.root(), &visitor).entries, walk(t.root(), &visitor).entries);
}

/// `StdReader` with some entries changed: a dataless child, a child whose own
/// metadata fails, and a directory on another device.
struct Altered {
    folder: PathBuf,
    deep: PathBuf,
}

impl Altered {
    fn alter(&self, dir: &Path, mut listing: DirListing) -> DirListing {
        for e in &mut listing.entries {
            match (dir == self.folder, dir == self.deep, e.name.to_str()) {
                (true, _, Some("dataless")) => e.flags |= scan::reader::SF_DATALESS,
                (true, _, Some("broken")) => e.error = Some(ddc_core::platform::errno::EACCES),
                (true, _, Some("volume")) | (_, true, Some("mnt")) => e.dev ^= 0x5a5a,
                _ => {}
            }
        }
        listing
    }
}

impl DirReader for Altered {
    fn read(
        &self,
        dir: &Path,
        expect: Option<(u64, u64)>,
        want_private: bool,
        times: Times,
    ) -> Result<DirListing, ReadError> {
        StdReader.read(dir, expect, want_private, times).map(|l| self.alter(dir, l))
    }
}

#[test]
fn listed_entries_keep_what_the_reader_could_not_measure() {
    if common::unsupported("listed_entries_keep_what_the_reader_could_not_measure") {
        return;
    }
    let t = TreeBuilder::new();
    let region = t.dir("Downloads");
    t.file("Downloads/dataless/f", 1_000);
    t.file("Downloads/broken", 1_000);
    t.file("Downloads/volume/f", 1_000);
    t.file("Downloads/deep/mnt/f", 1_000);
    t.file("Downloads/deep/kept", 8_000);
    t.file("Downloads/fine", 2_000);
    let visitor =
        TestVisitor { region_paths: vec![region.clone()], entry_paths: vec![region.clone()], ..TestVisitor::default() };
    let reader = Altered { folder: region.clone(), deep: t.path("Downloads/deep") };

    let out = scan::walk_with_reader(
        &[t.root().to_path_buf()],
        &visitor,
        &opts(ReaderKind::Std),
        &Progress::default(),
        &reader,
    )
    .expect("walk");

    assert!(entry(&out, &region, "dataless").flags & scan::reader::SF_DATALESS != 0);
    assert_eq!(entry(&out, &region, "broken").error, Some(ddc_core::platform::errno::EACCES));
    assert_eq!(entry(&out, &region, "volume").other_fs, 1);
    let deep = entry(&out, &region, "deep");
    assert_eq!(deep.other_fs, 1, "a mount point deeper inside");
    assert_eq!(deep.bytes, alloc(&t.path("Downloads/deep/kept")) + folders(&[t.path("Downloads/deep")]));
    let fine = entry(&out, &region, "fine");
    assert_eq!((fine.other_fs, fine.skipped, fine.errors), (0, 0, 0));
    assert_eq!(
        fine.modified,
        Some(ddc_core::platform::meta::mtime(&std::fs::symlink_metadata(t.path("Downloads/fine")).unwrap()))
    );
}

/// A visitor that prunes given paths (story "Faster clean: re-check only what
/// the selection needs").
struct PruningVisitor {
    inner: TestVisitor,
    pruned: Vec<PathBuf>,
}

impl Visitor for PruningVisitor {
    fn before_read(&self, dir: &DirCtx<'_>) -> Pre {
        self.inner.before_read(dir)
    }

    fn prunes(&self, dir: &Path) -> bool {
        self.pruned.iter().any(|p| p == dir)
    }
}

/// Records every directory the walk opens.
#[derive(Default)]
struct Opened(std::sync::Mutex<Vec<PathBuf>>);

impl DirReader for Opened {
    fn read(
        &self,
        dir: &Path,
        expect: Option<(u64, u64)>,
        want_private: bool,
        times: Times,
    ) -> Result<DirListing, ReadError> {
        self.0.lock().unwrap().push(dir.to_path_buf());
        StdReader.read(dir, expect, want_private, times)
    }
}

#[test]
fn a_pruned_directory_inside_a_claim_is_never_opened_and_counts_nowhere() {
    if common::unsupported("a_pruned_directory_inside_a_claim_is_never_opened_and_counts_nowhere") {
        return;
    }
    let t = TreeBuilder::new();
    t.file("claimed/kept/a.bin", 4096);
    t.file("claimed/left/deep/b.bin", 8192);
    t.file("outside/c.bin", 4096);
    let visitor = PruningVisitor {
        inner: TestVisitor { claim_paths: vec![t.path("claimed")], ..TestVisitor::default() },
        pruned: vec![t.path("claimed/left"), t.path("outside")],
    };
    let reader = Opened::default();
    let out = scan::walk_with_reader(
        &[t.root().to_path_buf()],
        &visitor,
        &opts(ReaderKind::Std),
        &Progress::default(),
        &reader,
    )
    .unwrap();

    let opened = reader.0.into_inner().unwrap();
    assert!(!opened.iter().any(|d| d.starts_with(t.path("claimed/left")) || d.starts_with(t.path("outside"))));
    assert_eq!(out.pruned, 2);
    assert_eq!((out.skipped_total, out.errors_total), (0, 0), "pruning is neither a skip nor an error");
    let claimed = owner(&out, &t.path("claimed"));
    assert!(!claimed.incomplete(), "a pruned directory does not make its owner incomplete");
    let read = folders(&[t.path("claimed"), t.path("claimed/kept")]);
    assert_eq!((claimed.files, claimed.dirs, claimed.bytes), (1, 2, alloc(&t.path("claimed/kept/a.bin")) + read));
}

/// Marks the entries named `mount-*` as another mount and `auto-*` as an
/// automount point, as the Linux reader does, and records every folder read.
#[derive(Default)]
struct Mounts(std::sync::Mutex<Vec<PathBuf>>);

impl DirReader for Mounts {
    fn read(
        &self,
        dir: &Path,
        expect: Option<(u64, u64)>,
        want_private: bool,
        times: Times,
    ) -> Result<DirListing, ReadError> {
        self.0.lock().unwrap().push(dir.to_path_buf());
        let mut listing = StdReader.read(dir, expect, want_private, times)?;
        for e in &mut listing.entries {
            let name = e.name.to_string_lossy();
            if name.starts_with("mount-") {
                e.boundary = Some(Boundary::Mount);
            } else if name.starts_with("auto-") {
                e.boundary = Some(Boundary::Automount);
            }
        }
        Ok(listing)
    }
}

/// Safety ADR, amendment of 2026-11-01: a mount the device does not tell
/// apart (a bind mount) and an automount point are never opened, are
/// skipped with their reasons, and leave the finding around them incomplete,
/// which the guard refuses.
#[test]
fn a_mount_or_an_automount_point_is_never_opened_and_leaves_its_finding_incomplete() {
    if common::unsupported("a_mount_or_an_automount_point_is_never_opened_and_leaves_its_finding_incomplete") {
        return;
    }
    let t = TreeBuilder::new();
    t.file("claimed/mount-data/a.bin", 4096);
    t.file("claimed/kept.bin", 4096);
    t.file("auto-share/b.bin", 4096);
    t.file("claimed/mount-file.bin", 4096);
    let visitor = TestVisitor { claim_paths: vec![t.path("claimed")], ..TestVisitor::default() };
    let reader = Mounts::default();
    let out = scan::walk_with_reader(
        &[t.root().to_path_buf()],
        &visitor,
        &opts(ReaderKind::Std),
        &Progress::default(),
        &reader,
    )
    .unwrap();

    let opened = reader.0.into_inner().unwrap();
    assert!(!opened.iter().any(|d| d.starts_with(t.path("claimed/mount-data")) || d.starts_with(t.path("auto-share"))));
    let mut skipped: Vec<_> = out.skipped.iter().map(|s| (s.path.clone(), s.reason)).collect();
    skipped.sort();
    assert_eq!(
        skipped,
        vec![
            (t.path("auto-share"), scan::SkipReason::Automount),
            (t.path("claimed/mount-data"), scan::SkipReason::OtherFilesystem),
            (t.path("claimed/mount-file.bin"), scan::SkipReason::OtherFilesystem),
        ]
    );
    let claimed = owner(&out, &t.path("claimed"));
    assert!(claimed.incomplete(), "a mount inside makes the finding incomplete");
    assert_eq!(claimed.files, 1, "neither the mount's file nor a bind-mounted file is counted");
}

/// Records the name of every thread that opens a directory.
#[derive(Default)]
struct ThreadNames(std::sync::Mutex<std::collections::BTreeSet<String>>);

impl DirReader for ThreadNames {
    fn read(
        &self,
        dir: &Path,
        expect: Option<(u64, u64)>,
        want_private: bool,
        times: Times,
    ) -> Result<DirListing, ReadError> {
        let name = std::thread::current().name().unwrap_or("unnamed").to_owned();
        self.0.lock().unwrap().insert(name);
        StdReader.read(dir, expect, want_private, times)
    }
}

/// The scheduled scan asks for three threads (`background_walk` in the CLI);
/// the walk then opens directories on no more than three of its own.
#[test]
fn a_walk_asked_for_three_threads_reads_on_at_most_three() {
    if common::unsupported("a_walk_asked_for_three_threads_reads_on_at_most_three") {
        return;
    }
    let t = TreeBuilder::new();
    for i in 0..200 {
        t.file(&format!("d{i}/e/f.bin"), 4096);
    }
    let reader = ThreadNames::default();
    let opts = WalkOptions { threads: Some(3), ..opts(ReaderKind::Std) };
    scan::walk_with_reader(&[t.root().to_path_buf()], &TestVisitor::default(), &opts, &Progress::default(), &reader)
        .unwrap();
    let names = reader.0.into_inner().unwrap();
    assert!(!names.is_empty() && names.len() <= 3, "{names:?}");
    assert!(names.iter().all(|n| n.starts_with("ddc-walk-")), "{names:?}");
}

// ---- story "Review window: sort a group's rows by name, size or last change" ----

/// [`TestVisitor`] whose claims ask for times, and not for private sizes:
/// a `.venv` or `node_modules` claimed after its listing.
struct Timed(TestVisitor);

impl Visitor for Timed {
    fn before_read(&self, dir: &DirCtx<'_>) -> Pre {
        self.0.before_read(dir)
    }

    fn after_read(&self, dir: &DirCtx<'_>, listing: &DirListing) -> Post {
        self.0.after_read(dir, listing)
    }

    fn wants_private(&self, _dir: &DirCtx<'_>, _tag: Tag) -> bool {
        false
    }

    fn wants_times(&self, _dir: &DirCtx<'_>, _tag: Tag) -> bool {
        true
    }
}

#[test]
fn a_claim_holds_the_newest_time_inside_and_a_region_none() {
    if common::unsupported("a_claim_holds_the_newest_time_inside_and_a_region_none") {
        return;
    }
    let t = TreeBuilder::new();
    t.file("proj/claimme/a", 1_000);
    t.file("proj/claimme/deep/er/b", 1_000);
    let outside = t.file("store/linked", 1_000);
    t.hardlink(&outside, "proj/claimme/deep/linked");
    t.file("known/top", 1_000);
    t.file("known/sub/c", 1_000);
    t.file("area/d", 1_000);
    let visitor = Timed(TestVisitor {
        claim_paths: vec![t.path("known")],
        region_paths: vec![t.path("area")],
        ..TestVisitor::default()
    });

    for reader in [ReaderKind::Bulk, ReaderKind::Std] {
        t.age("", OLD);
        // Three levels down in a claim made after its listing.
        t.age("proj/claimme/deep/er/b", OLD + 10);
        // A direct child of a claim made before its listing.
        t.age("known/top", OLD + 20);
        let out = walk_with(t.root(), &visitor, reader);
        assert_eq!(owner(&out, &t.path("proj/claimme")).modified, Some(OLD + 10), "{reader:?}");
        assert_eq!(owner(&out, &t.path("known")).modified, Some(OLD + 20), "{reader:?}");
        assert_eq!(owner(&out, &t.path("area")).modified, None, "a region has no date ({reader:?})");

        // A hard-linked file's bytes go to the inode table; its time still counts.
        t.age("store/linked", OLD + 30);
        let out = walk_with(t.root(), &visitor, reader);
        assert_eq!(owner(&out, &t.path("proj/claimme")).modified, Some(OLD + 30), "{reader:?}");
        // The directory's own time counts too.
        t.touch("proj/claimme", OLD + 40);
        let out = walk_with(t.root(), &visitor, reader);
        assert_eq!(owner(&out, &t.path("proj/claimme")).modified, Some(OLD + 40), "{reader:?}");
    }

    // Off, or not asked for: no claim reads a time.
    let off = WalkOptions { claim_times: false, ..opts(ReaderKind::Bulk) };
    let out = scan::walk(&[t.root().to_path_buf()], &visitor, &off, &Progress::default()).unwrap();
    assert!(out.owners.iter().all(|o| o.modified.is_none()), "{:#?}", out.owners);
    let out = walk(t.root(), &visitor.0);
    assert!(out.owners.iter().all(|o| o.modified.is_none()), "{:#?}", out.owners);
}

#[test]
fn a_claim_whose_root_cannot_be_opened_has_no_time_and_an_unreadable_folder_inside_keeps_its_own() {
    if common::unsupported(
        "a_claim_whose_root_cannot_be_opened_has_no_time_and_an_unreadable_folder_inside_keeps_its_own",
    ) {
        return;
    }
    let t = TreeBuilder::new();
    t.file("shut/a", 1_000);
    t.file("known/a", 1_000);
    t.file("known/locked/b", 1_000);
    t.age("", OLD);
    t.age("known/locked", OLD + 50);
    let shut = t.lock("shut");
    t.lock("known/locked");
    if std::fs::read_dir(&shut).is_ok() {
        eprintln!("SKIPPED: running as root, permissions are not enforced");
        return;
    }
    let visitor = Timed(TestVisitor { claim_paths: vec![shut.clone(), t.path("known")], ..TestVisitor::default() });

    for reader in [ReaderKind::Bulk, ReaderKind::Std] {
        let out = walk_with(t.root(), &visitor, reader);
        assert_eq!(owner(&out, &shut).modified, None, "{reader:?}");
        let known = owner(&out, &t.path("known"));
        assert!(known.incomplete(), "{reader:?}");
        assert_eq!(known.modified, Some(OLD + 50), "the parent's listing has its time ({reader:?})");
    }
}

/// [`StdReader`] that cannot list `dir` with times.
struct NoTimesAt(PathBuf);

impl DirReader for NoTimesAt {
    fn read(
        &self,
        dir: &Path,
        expect: Option<(u64, u64)>,
        want_private: bool,
        times: Times,
    ) -> Result<DirListing, ReadError> {
        if dir == self.0 && times != Times::None {
            return Err(ReadError::new(ReadErrorKind::PermissionDenied, Some(ddc_core::platform::errno::EACCES)));
        }
        StdReader.read(dir, expect, want_private, times)
    }
}

#[test]
fn a_failed_re_read_for_the_times_alone_leaves_a_lower_bound_and_no_error() {
    if common::unsupported("a_failed_re_read_for_the_times_alone_leaves_a_lower_bound_and_no_error") {
        return;
    }
    let t = TreeBuilder::new();
    t.file("claimme/new", 1_000);
    t.file("claimme/sub/old", 1_000);
    t.age("", OLD);
    t.age("claimme/new", OLD + 60);
    let visitor = Timed(TestVisitor::default());
    let claim = t.path("claimme");
    let walk_through = |reader: &dyn DirReader| {
        let roots = [t.root().to_path_buf()];
        scan::walk_with_reader(&roots, &visitor, &opts(ReaderKind::Std), &Progress::default(), reader).unwrap()
    };

    let out = walk_through(&StdReader);
    assert_eq!(owner(&out, &claim).modified, Some(OLD + 60), "listed again with times");

    let out = walk_through(&NoTimesAt(claim.clone()));
    let c = owner(&out, &claim);
    assert_eq!(c.modified, Some(OLD), "the root's own files are missing, never the subtree");
    assert!(!c.incomplete(), "{c:#?}");
    assert_eq!((out.errors_total, c.bytes), (0, reference_alloc(&claim)));
}
