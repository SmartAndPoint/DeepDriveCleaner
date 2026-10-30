//! `hostile`: a scan completes while the tree changes under it; an entry
//! that vanishes between listing and reading is counted as vanished, and a
//! folder replaced under the same name is skipped as changed. Either makes
//! its finding incomplete, neither is an error.

// Unix only: the hostile tree swaps folders through Unix symlinks and errno numbers.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

use ddc_core::rules::{Catalog, RuleVisitor};
use ddc_core::scan::reader::{DirListing, DirReader, ReadError, StdReader, Times};
use ddc_core::scan::{self, OwnerResult, Progress, ReaderKind, SkipReason, WalkOptions, WalkOutput};
use ddc_loadgen::profiles::{Profile, hostile};
use ddc_loadgen::{generate, target, verify};

const S: u64 = 10_000;

/// Stops the mutator when dropped, so a failing scan ends the test instead
/// of waiting for a thread that never stops.
struct Stop<'a>(&'a AtomicBool);

impl Drop for Stop<'_> {
    fn drop(&mut self) {
        self.0.store(true, Relaxed);
    }
}

#[test]
fn a_scan_completes_while_the_tree_changes() {
    if ddc_loadgen::unsupported("a_scan_completes_while_the_tree_changes") {
        return;
    }
    let dir = target::Scratch::new().unwrap();
    let g = generate(Profile::Hostile, S, 1, dir.path(), None).unwrap();
    let stop = AtomicBool::new(false);
    let (changes, diffs) = std::thread::scope(|s| {
        let mutator = s.spawn(|| hostile::mutate(&g.layout.tree, 7, &stop));
        let diffs: Vec<String> = {
            let _stop = Stop(&stop);
            (0..3).flat_map(|_| verify::subset(&g.manifest, &g.layout, &g.home).unwrap()).collect()
        };
        (mutator.join().unwrap(), diffs)
    });
    let path = dir.path().to_owned();
    drop(dir);
    assert!(!path.exists(), "{} is left behind", path.display());
    assert!(changes > 0, "the mutator changed nothing");
    assert!(diffs.is_empty(), "{}", diffs.join("\n"));
}

/// [`StdReader`], with `gone` deleted just before it is read, the entry
/// `flaky/x` reported as vanished between the listing and its metadata, and
/// `swapped` replaced by a new folder of the same name before it is read.
struct Vanishing {
    gone: PathBuf,
    flaky: PathBuf,
    swapped: PathBuf,
}

impl DirReader for Vanishing {
    #[allow(clippy::disallowed_methods)] // deletes a folder of its own test tree
    fn read(
        &self,
        dir: &Path,
        expect: Option<(u64, u64)>,
        want_private: bool,
        times: Times,
    ) -> Result<DirListing, ReadError> {
        if dir == self.gone {
            std::fs::remove_dir_all(dir).unwrap();
        }
        if dir == self.swapped {
            // Its parent is already listed: the old folder is not seen again.
            std::fs::rename(dir, dir.with_extension("old")).unwrap();
            std::fs::create_dir(dir).unwrap();
        }
        let mut listing = StdReader.read(dir, expect, want_private, times)?;
        if dir == self.flaky {
            for e in listing.entries.iter_mut().filter(|e| e.name == "x") {
                e.error = Some(libc::ENOENT);
            }
        }
        Ok(listing)
    }
}

fn owner<'o>(out: &'o WalkOutput, path: &Path) -> &'o OwnerResult {
    out.owners.iter().find(|o| o.path == path).unwrap_or_else(|| panic!("no finding at {}", path.display()))
}

#[test]
fn a_vanished_or_replaced_entry_makes_its_finding_incomplete() {
    if ddc_loadgen::unsupported("a_vanished_or_replaced_entry_makes_its_finding_incomplete") {
        return;
    }
    let dir = target::Scratch::new().unwrap();
    let g = generate(Profile::Hostile, S, 1, dir.path(), None).unwrap();
    let tree = &g.layout.tree;
    let catalog = Catalog::builtin(&g.home);
    let visitor = RuleVisitor::new(&catalog, Vec::new());
    let reader = Vanishing {
        gone: tree.join("hostile/gone/node_modules/a"),
        flaky: tree.join("hostile/flaky/node_modules"),
        swapped: tree.join("hostile/flaky/node_modules/keep"),
    };
    let opts = WalkOptions { reader: ReaderKind::Std, ..WalkOptions::default() };
    let out =
        scan::walk_with_reader(std::slice::from_ref(tree), &visitor, &opts, &Progress::default(), &reader).unwrap();

    assert_eq!(out.vanished, 2, "the folder deleted before its read and the entry gone before its metadata");
    for project in ["hostile/gone", "hostile/flaky"] {
        let o = owner(&out, &tree.join(project).join("node_modules"));
        assert!(o.incomplete(), "{project}: {o:?}");
        assert_eq!(o.errors, 0, "{project}: a vanished entry is not an error");
    }
    // The replaced folder is skipped as changed, not as vanished.
    assert_eq!(out.skipped_total, 1);
    assert!(
        out.skipped.iter().any(|s| s.path == reader.swapped && s.reason == SkipReason::Changed),
        "{:?}",
        out.skipped
    );
    // The two locked folders are the only errors.
    assert_eq!(out.errors_total, 2);
    assert!(owner(&out, &tree.join("hostile/sealed/node_modules")).errors > 0);
}
