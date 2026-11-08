//! The Windows Recycle Bin on the lab's test volume (safety ADR §5, the
//! Recycle Bin on Windows, "Tests" and "Settled at 10b's lab"; story
//! "Windows scanner and rule catalog", step 10b): a small `MaxCapacity`
//! refuses an item too large and one that does not fit what is left, and
//! the shell reads the value on each move; the size compared is what the
//! shell counts (a sparse file's length, each name of a hard-linked file),
//! and a folder over the maximum is stopped by the sink when moved without
//! asking; a clean through the system trash moves what fits and skips the
//! rest whole (end to end, under `unsupported()`, proven in a dry flip).
//! Elsewhere every test here skips, in the environment group of
//! `.config/native-skips/windows.txt`.
#![cfg(target_os = "windows")]
#![allow(clippy::disallowed_methods)] // the test's own files on its own volume

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use common::TreeBuilder;
use common::fakes::{FakeCommands, FakeProcs};
use common::recycle::{Restore, set_max_capacity, system_root, take_back, test_volume};
use ddc_core::env::Env;
use ddc_core::exec::{self, Consent, ExecOptions, MemoryJournal, Outcome, Ports, SkipReason, SystemTrash};
use ddc_core::plan::{PlanOptions, build_plan};
use ddc_core::platform::fsops::DirFd;
use ddc_core::platform::trash::{self, Budget};
use ddc_core::platform::types::{RecycleBinCause, TrashAsk, TrashDone, TrashError};
use ddc_core::report::{self, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::scan::{Progress, WalkOptions};

fn item(dir: &Path, name: &str, mib: usize) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, vec![b'x'; mib << 20]).unwrap();
    path
}

fn ask(path: &Path) -> TrashAsk<'_> {
    TrashAsk { name: path.file_name().unwrap(), path, bytes: fs::metadata(path).unwrap().len(), is_dir: false }
}

fn cause(r: Result<(), TrashError>) -> Option<RecycleBinCause> {
    match r {
        Err(TrashError::RecycleBinRefuses(c)) => Some(c),
        _ => None,
    }
}

fn id(path: &Path) -> (u64, u64) {
    ddc_core::platform::meta::lstat(path).unwrap().id()
}

#[test]
fn a_small_bin_refuses_an_item_too_large_and_one_that_does_not_fit_what_is_left() {
    const TEST: &str = "a_small_bin_refuses_an_item_too_large_and_one_that_does_not_fit_what_is_left";
    let Some(root) = test_volume(TEST) else { return };
    let dir = root.join("ddc-recycle-test");
    fs::create_dir_all(&dir).unwrap();
    let restore = Restore::new(TEST, &root);
    let key = restore.key().to_owned();
    let folder = DirFd::open(&dir).unwrap();

    set_max_capacity(&key, Some(1));
    let big = item(&dir, "big.bin", 2);
    assert_eq!(cause(trash::admit(&mut Budget::default(), &folder, &[ask(&big)])), Some(RecycleBinCause::TooLarge));

    set_max_capacity(&key, Some(4));
    let (a, b) = (item(&dir, "a.bin", 3), item(&dir, "b.bin", 3));
    let mut budget = Budget::default();
    assert!(trash::admit(&mut budget, &folder, &[ask(&a)]).is_ok(), "3 MiB of a 4 MiB bin");
    assert_eq!(cause(trash::admit(&mut budget, &folder, &[ask(&b)])), Some(RecycleBinCause::BinFull), "1 MiB left");

    // The shell reads the value on each move: past it, never in the bin;
    // under it, in the bin.
    set_max_capacity(&key, Some(1));
    let past = trash::trash_item(&folder, big.file_name().unwrap(), &big, id(&big));
    eprintln!("LAB L0 too large, the move itself: {past:?}; source {}", if big.exists() { "whole" } else { "gone" });
    let refused = matches!(past, Err(TrashError::RecycleBinRefuses(_)));
    assert!(
        matches!(past, Ok(TrashDone::RemovedNotRecycled)) || (refused && big.exists()),
        "an item over MaxCapacity is destroyed and said so, or refused whole: {past:?}"
    );
    // 3 MiB: destroyed at 1, moved only if the shell read the 4 set now.
    set_max_capacity(&key, Some(4));
    let small = item(&dir, "small.bin", 3);
    let done =
        trash::trash_item(&folder, small.file_name().unwrap(), &small, id(&small)).expect("the move under the maximum");
    let TrashDone::Moved(Some(place)) = done else { panic!("not moved: {done:?}") };
    take_back(&place.trash.join(&place.name));
    drop(restore);
    let _ = fs::remove_dir_all(&dir);
}

/// Denies everyone the listing of `folder` (`/deny`), or takes that back
/// (`/remove:d`).
fn icacls(folder: &Path, how: &str) {
    let rule = if how == "/deny" { "*S-1-1-0:(RD)" } else { "*S-1-1-0" };
    let out = Command::new(system_root().join(r"System32\icacls.exe"))
        .arg(folder)
        .args([how, rule])
        .output()
        .expect("icacls runs");
    assert!(out.status.success(), "icacls {how}: {}", String::from_utf8_lossy(&out.stdout));
}

/// A sparse file of `mib` MiB with nothing allocated.
fn sparse(path: &Path, mib: u64) {
    fs::write(path, b"").unwrap();
    let made = Command::new(system_root().join(r"System32\fsutil.exe"))
        .args(["sparse", "setflag"])
        .arg(path)
        .output()
        .expect("fsutil runs");
    assert!(made.status.success(), "fsutil: {}", String::from_utf8_lossy(&made.stdout));
    fs::OpenOptions::new().write(true).open(path).unwrap().set_len(mib << 20).unwrap();
}

#[test]
fn the_size_compared_is_what_the_shell_counts_and_a_folder_over_the_maximum_stays_whole() {
    const TEST: &str = "the_size_compared_is_what_the_shell_counts_and_a_folder_over_the_maximum_stays_whole";
    let Some(root) = test_volume(TEST) else { return };
    let dir = root.join("ddc-recycle-size-test");
    fs::create_dir_all(&dir).unwrap();
    let restore = Restore::new(TEST, &root);
    for left in ["sparse.bin", "holder", "names", "shut"] {
        if left == "shut" && dir.join("shut").join("sub").exists() {
            icacls(&dir.join("shut").join("sub"), "/remove:d");
        }
        let _ = fs::remove_dir_all(dir.join(left)).or_else(|_| fs::remove_file(dir.join(left)));
    }
    set_max_capacity(restore.key(), Some(4));
    let folder = DirFd::open(&dir).unwrap();
    let mut budget = Budget::default();

    // A sparse file: 8 MiB long, nothing allocated; the shell would destroy it.
    let file = dir.join("sparse.bin");
    sparse(&file, 8);
    let allocated = ddc_core::platform::meta::lstat(&file).unwrap().alloc;
    assert!(allocated < 4 << 20, "the test needs an allocated size that fits");
    let file_ask = TrashAsk { name: file.file_name().unwrap(), path: &file, bytes: allocated, is_dir: false };
    assert_eq!(cause(trash::admit(&mut budget, &folder, &[file_ask])), Some(RecycleBinCause::TooLarge));

    // A folder holding one, and a folder of two names of one 3 MiB file.
    let holder = dir.join("holder");
    fs::create_dir(&holder).unwrap();
    sparse(&holder.join("sparse.bin"), 8);
    let names = dir.join("names");
    fs::create_dir(&names).unwrap();
    fs::write(names.join("a"), vec![b'x'; 3 << 20]).unwrap();
    fs::hard_link(names.join("a"), names.join("b")).unwrap();
    for item in [&holder, &names] {
        let ask = TrashAsk { name: item.file_name().unwrap(), path: item, bytes: 3 << 20, is_dir: true };
        assert_eq!(
            cause(trash::admit(&mut budget, &folder, &[ask])),
            Some(RecycleBinCause::TooLarge),
            "{}",
            item.display()
        );
        // Moved without asking: the sink stops the shell at the first child.
        let moved = trash::trash_item(&folder, item.file_name().unwrap(), item, id(item));
        if let Ok(TrashDone::Moved(Some(place))) = &moved {
            take_back(&place.trash.join(&place.name));
        }
        assert!(matches!(moved, Err(TrashError::RecycleBinRefuses(RecycleBinCause::TooLarge))), "{moved:?}");
        assert!(item.exists(), "{} stays whole", item.display());
    }
    assert!(holder.join("sparse.bin").exists(), "the sparse file is still in its folder");

    // A folder whose walk cannot finish: refused held-open, never `Other`,
    // which the clean would meet by moving it unchecked.
    let shut = dir.join("shut");
    fs::create_dir_all(shut.join("sub")).unwrap();
    icacls(&shut.join("sub"), "/deny");
    let ask = TrashAsk { name: shut.file_name().unwrap(), path: &shut, bytes: 0, is_dir: true };
    let answer = trash::admit(&mut Budget::default(), &folder, &[ask]);
    icacls(&shut.join("sub"), "/remove:d");
    assert!(matches!(answer, Err(TrashError::HeldOpen)), "{answer:?}");
    assert!(names.join("a").exists() && names.join("b").exists(), "both names are still there");
    drop(restore);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_clean_through_the_recycle_bin_moves_what_fits_and_skips_a_folder_too_large_whole() {
    const TEST: &str = "a_clean_through_the_recycle_bin_moves_what_fits_and_skips_a_folder_too_large_whole";
    if common::unsupported("a_clean_through_the_recycle_bin_moves_what_fits_and_skips_a_folder_too_large_whole") {
        return;
    }
    let Some(root) = test_volume(TEST) else { return };
    let dir = root.join("ddc-recycle-clean-test");
    let restore = Restore::new(TEST, &root);
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    set_max_capacity(restore.key(), Some(4));
    let t = TreeBuilder::new_in(&dir);
    let data = tempfile::tempdir().unwrap();
    let now = SystemTime::now();
    // Two venvs with no lock, `review` items the clean moves to the bin: one
    // over the bin's maximum.
    let venv = |rel: &str, len: usize| {
        t.write(&format!("{rel}/.venv/pyvenv.cfg"), "x");
        t.file(&format!("{rel}/.venv/lib/x.py"), len);
        t.path(&format!("{rel}/.venv"))
    };
    let big = venv("big", 5 << 20);
    let small = venv("small", 20_000);
    let env = Env::with_paths(t.root(), data.path(), &data.path().join("logs"), now).unwrap();
    let opts = ScanOptions { walk: WalkOptions { threads: Some(2), ..Default::default() }, ..Default::default() };
    let scan = report::scan(&env, &opts, &Progress::default()).unwrap();
    let catalog = Catalog::for_home(t.root());
    let mut plan = build_plan(&scan, &catalog, &PlanOptions::default());
    plan.items.iter_mut().for_each(|i| i.selected = true);
    let mut journal = MemoryJournal::default();
    let run = exec::execute(
        &env,
        &catalog,
        &plan,
        &Consent::Interactive,
        Ports {
            commands: &FakeCommands::default(),
            trash: &SystemTrash::default(),
            procs: &FakeProcs::new(vec![]),
            journal: &mut journal,
        },
        &ExecOptions::default(),
    )
    .unwrap();
    // ddc's own items leave the bin by the names it gave, before any check.
    for line in &journal.0 {
        if let Some(to) = &line.trash {
            take_back(&to.trash.join(&to.name));
        }
    }
    let outcome = |p: &Path| run.items.iter().find(|i| i.path == p).map(|i| &i.outcome);
    assert_eq!(
        outcome(&big),
        Some(&Outcome::Skipped(SkipReason::RecycleBinRefuses { cause: RecycleBinCause::TooLarge })),
        "{:#?}",
        run.items
    );
    assert!(big.join("lib").join("x.py").exists(), "the folder too large stays whole");
    assert_eq!(outcome(&small), Some(&Outcome::Trashed), "{:#?}", run.items);
    assert!(!small.exists());
    assert!(journal.0.iter().any(|l| l.path == small && l.trash.is_some()), "the journal names the bin");
    drop(restore);
    drop(t);
    let _ = fs::remove_dir_all(&dir);
}
