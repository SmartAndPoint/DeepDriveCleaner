//! Special files and the Trash on exFAT (safety ADR, amendment of
//! 2026-11-04; story "Linux file kinds: special files, immutable flags,
//! sparse files and odd names", DDC-010), on the fixtures and mounts of the
//! Gate's linux-mounts job. Elsewhere every test here skips, in the
//! environment group of `.config/native-skips/linux.txt`.
#![cfg(target_os = "linux")]

mod common;

use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::time::SystemTime;

use common::fakes::{CrossingTrash, FakeCommands, FakeProcs};
use common::{Mount, Prepared, TreeBuilder};
use ddc_core::class::RiskClass;
use ddc_core::env::Env;
use ddc_core::exec::{self, Consent, ExecOptions, MemoryJournal, Outcome, Ports};
use ddc_core::plan::{PlanOptions, build_plan};
use ddc_core::platform::types::TrashError;
use ddc_core::report::{self, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::scan::{Progress, WalkOptions};
use rustix::fs::inotify;

/// An inotify watch for opens of `path`.
fn watch_opens(path: &Path) -> OwnedFd {
    let fd = inotify::init(inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK).unwrap();
    inotify::add_watch(&fd, path, inotify::WatchFlags::OPEN).unwrap();
    fd
}

/// Whether the watched file was opened since [`watch_opens`].
fn opened(watch: &OwnedFd) -> bool {
    let mut buf = [MaybeUninit::<u8>::uninit(); 4096];
    let mut reader = inotify::Reader::new(watch, &mut buf);
    loop {
        match reader.next() {
            Ok(event) if event.events().contains(inotify::ReadFlags::OPEN) => return true,
            Ok(_) => {}
            Err(rustix::io::Errno::AGAIN) => return false,
            Err(e) => panic!("inotify: {e}"),
        }
    }
}

/// A device node inside a finding is counted at nothing, never opened by
/// the scan, the plan or the clean, and unlinked with its folder by a
/// permanent delete. The open is what reads a device: inotify sees every
/// open of the node.
#[test]
fn a_device_node_in_a_finding_is_counted_never_opened_and_unlinked() {
    let Some(home) =
        common::prepared("a_device_node_in_a_finding_is_counted_never_opened_and_unlinked", Prepared::DeviceTree)
    else {
        return;
    };
    let nm = home.join("p/node_modules");
    let node = nm.join("dev/null");
    let watch = watch_opens(&node);
    let data = tempfile::tempdir().unwrap();
    let env = Env::with_paths(&home, data.path(), &data.path().join("logs"), SystemTime::now()).unwrap();
    let opts = ScanOptions { walk: WalkOptions { threads: Some(2), ..Default::default() }, ..Default::default() };

    let scan = report::scan(&env, &opts, &Progress::default()).unwrap();
    let finding = scan.findings.iter().find(|f| f.path == nm).expect("the node_modules finding");
    assert!(finding.bytes < 64 * 1024, "the node allocates nothing: {}", finding.bytes);
    assert!(!finding.incomplete, "a device is no reason to stop");
    let catalog = Catalog::builtin(&home);
    let plan = build_plan(&scan, &catalog, &PlanOptions::default());
    let mut journal = MemoryJournal::default();
    let consent = Consent::AssumeYes { classes: vec![RiskClass::Safe, RiskClass::Rebuildable] };
    let ports = Ports {
        commands: &FakeCommands::default(),
        trash: &CrossingTrash,
        procs: &FakeProcs::new(vec![]),
        journal: &mut journal,
    };
    let r = exec::execute(&env, &catalog, &plan, &consent, ports, &ExecOptions::default()).unwrap();

    let item = r.items.iter().find(|i| i.path == nm).expect("node_modules was cleaned");
    assert_eq!(item.outcome, Outcome::Deleted);
    assert!(!nm.exists());
    assert!(!opened(&watch), "the device was opened");
}

/// A folder holding a device node moves into the Trash of its own tmpfs by
/// a rename, the node with it, never opened and never copied: the dry run
/// says it would, the move keeps the folder's identity.
#[test]
fn a_device_node_moves_into_the_trash_unopened() {
    use ddc_core::platform::trash;
    let Some(dir) = common::prepared("a_device_node_moves_into_the_trash_unopened", Prepared::DeviceTrash) else {
        return;
    };
    let item = dir.join("cache");
    let node = item.join("null");
    let watch = watch_opens(&node);
    let id = ddc_core::platform::meta::lstat(&item).unwrap().id();
    let parent = ddc_core::platform::fsops::DirFd::open(&dir).unwrap();
    let name = item.file_name().unwrap();

    trash::would_trash(&parent, name, &item).unwrap();
    let place = trash::trash_item(&parent, name, &item, id).unwrap().expect("Linux tells the place");

    let landed = place.trash.join("files").join(&place.name);
    assert_eq!(ddc_core::platform::meta::lstat(&landed).unwrap().id(), id, "moved, not copied");
    use std::os::unix::fs::FileTypeExt as _;
    assert!(std::fs::symlink_metadata(landed.join("null")).unwrap().file_type().is_char_device());
    assert!(!item.exists());
    assert!(!opened(&watch), "the device was opened");
}

/// exFAT takes its modes from the mount: a `.Trash-$uid` made there stays
/// open to others whatever `fchmod` says, so the move refuses as crossing.
/// The dry run says so before the folder exists, from the filesystem's
/// type, and after, from its mode (the first run on a fresh volume sees
/// both); the item stays.
#[test]
fn an_exfat_trash_that_stays_open_refuses_the_move() {
    use ddc_core::platform::trash;
    let Some(top) = common::mount("an_exfat_trash_that_stays_open_refuses_the_move", Mount::Exfat) else {
        return;
    };
    let tree = TreeBuilder::new_in(&top);
    let file = tree.file("cache/blob", 4096);
    let id = ddc_core::platform::meta::lstat(&file).unwrap().id();
    let folder = ddc_core::platform::fsops::DirFd::open(file.parent().unwrap()).unwrap();
    let name = file.file_name().unwrap();

    assert!(matches!(trash::would_trash(&folder, name, &file), Err(TrashError::CrossesFilesystem)), "before");
    assert!(matches!(trash::trash_item(&folder, name, &file, id), Err(TrashError::CrossesFilesystem)));
    assert!(file.exists(), "nothing moved");
    assert!(matches!(trash::would_trash(&folder, name, &file), Err(TrashError::CrossesFilesystem)), "after");
}
