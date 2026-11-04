//! Special files and the Trash on exFAT (safety ADR, amendment of
//! 2026-11-04; story "Linux file kinds: special files, immutable flags,
//! sparse files and odd names", DDC-010). What any user can make (a FIFO, a
//! socket) runs everywhere; the device nodes and exFAT come from the
//! fixtures and mounts of the Gate's linux-mounts job, and elsewhere those
//! tests skip, in the environment group of `.config/native-skips/linux.txt`.
#![cfg(target_os = "linux")]

mod common;

use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use common::fakes::{CrossingTrash, FakeCommands, FakeProcs};
use common::{Mount, Prepared, TreeBuilder};
use ddc_core::class::RiskClass;
use ddc_core::env::Env;
use ddc_core::exec::{self, Consent, ExecOptions, MemoryJournal, Outcome, Ports, SkipReason};
use ddc_core::plan::{Plan, PlanOptions, build_plan};
use ddc_core::platform::types::{TrashAsk, TrashDone, TrashError};
use ddc_core::report::{self, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::scan::{Progress, WalkOptions};
use rustix::fs::inotify;

/// One part of a move to the Trash, for `trash::admit`.
fn ask<'a>(name: &'a std::ffi::OsStr, path: &'a Path, is_dir: bool) -> TrashAsk<'a> {
    TrashAsk { name, path, bytes: 0, is_dir }
}

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

/// The scan and a clean of the plan for `home`, deleting (no Trash).
fn scan_and_clean(home: &Path) -> (ddc_core::model::ScanReport, exec::ExecReport) {
    let data = tempfile::tempdir().unwrap();
    let scan = scan_in(home, data.path());
    let plan = build_plan(&scan, &Catalog::builtin(home), &PlanOptions::default());
    let r = clean_in(home, data.path(), &plan);
    (scan, r)
}

/// The scan of `home`, its data in `data`.
fn scan_in(home: &Path, data: &Path) -> ddc_core::model::ScanReport {
    let env = Env::with_paths(home, data, &data.join("logs"), SystemTime::now()).unwrap();
    let opts = ScanOptions { walk: WalkOptions { threads: Some(2), ..Default::default() }, ..Default::default() };
    report::scan(&env, &opts, &Progress::default()).unwrap()
}

/// A clean of `plan` for `home` with `--yes`, deleting (no Trash).
fn clean_in(home: &Path, data: &Path, plan: &Plan) -> exec::ExecReport {
    let env = Env::with_paths(home, data, &data.join("logs"), SystemTime::now()).unwrap();
    let catalog = Catalog::builtin(home);
    let mut journal = MemoryJournal::default();
    let consent = Consent::AssumeYes { classes: vec![RiskClass::Safe, RiskClass::Rebuildable] };
    let ports = Ports {
        commands: &FakeCommands::default(),
        trash: &CrossingTrash,
        procs: &FakeProcs::new(vec![]),
        journal: &mut journal,
    };
    exec::execute(&env, &catalog, plan, &consent, ports, &ExecOptions::default()).unwrap()
}

/// An idle Node project at `rel` in `t`; its `node_modules`.
fn idle_node_project(t: &TreeBuilder, rel: &str) -> PathBuf {
    let old = SystemTime::now() - std::time::Duration::from_secs(60 * 86_400);
    for f in ["package.json", "package-lock.json"] {
        let path = t.write(&format!("{rel}/{f}"), "{}");
        std::fs::File::options().write(true).open(path).unwrap().set_modified(old).unwrap();
    }
    t.file(&format!("{rel}/node_modules/dep/index.js"), 20_000);
    t.path(&format!("{rel}/node_modules"))
}

/// A FIFO and a socket inside a finding are never opened by the scan, the
/// plan or a clean, which unlinks them with their folder: inotify sees
/// every open of either. Any user makes both, so this runs in every gate.
#[test]
fn a_fifo_and_a_socket_in_a_finding_are_never_opened() {
    let t = TreeBuilder::new();
    let old = SystemTime::now() - std::time::Duration::from_secs(60 * 86_400);
    for f in ["p/package.json", "p/package-lock.json"] {
        let path = t.write(f, "{}");
        std::fs::File::options().write(true).open(path).unwrap().set_modified(old).unwrap();
    }
    t.file("p/node_modules/dep/index.js", 20_000);
    let nm = t.path("p/node_modules");
    let fifo = nm.join("dep/pipe");
    rustix::fs::mknodat(rustix::fs::CWD, &fifo, rustix::fs::FileType::Fifo, rustix::fs::Mode::from_raw_mode(0o644), 0)
        .unwrap();
    let sock = nm.join("dep/dev.sock");
    drop(std::os::unix::net::UnixListener::bind(&sock).unwrap());
    let watches = [watch_opens(&fifo), watch_opens(&sock)];

    let (scan, r) = scan_and_clean(t.root());

    assert!(scan.findings.iter().any(|f| f.path == nm && !f.incomplete), "the finding, whole");
    let item = r.items.iter().find(|i| i.path == nm).expect("node_modules was cleaned");
    assert_eq!(item.outcome, Outcome::Deleted);
    assert!(!nm.exists());
    for (watch, what) in watches.iter().zip(["the FIFO", "the socket"]) {
        assert!(!opened(watch), "{what} was opened");
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

    let (scan, r) = scan_and_clean(&home);

    let finding = scan.findings.iter().find(|f| f.path == nm).expect("the node_modules finding");
    assert!(finding.bytes < 64 * 1024, "the node allocates nothing: {}", finding.bytes);
    assert!(!finding.incomplete, "a device is no reason to stop");
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

    trash::admit(&mut trash::Budget, &parent, &[ask(name, &item, true)]).unwrap();
    let TrashDone::Moved(Some(place)) = trash::trash_item(&parent, name, &item, id).unwrap() else {
        panic!("Linux moves and tells the place");
    };

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

    let admit = || trash::admit(&mut trash::Budget, &folder, &[ask(name, &file, false)]);
    assert!(matches!(admit(), Err(TrashError::CrossesFilesystem)), "before");
    assert!(matches!(trash::trash_item(&folder, name, &file, id), Err(TrashError::CrossesFilesystem)));
    assert!(file.exists(), "nothing moved");
    assert!(matches!(admit(), Err(TrashError::CrossesFilesystem)), "after");
}

/// On a case-insensitive folder, a protected folder named in another case
/// is the protected folder: `DOCUMENTS` in a plan is refused as
/// `Documents` is, by its folded path and by the identity the filesystem
/// resolves it to (safety ADR, amendment of 2026-11-04, §6).
fn a_protected_folder_in_another_case_is_refused_on(test: &str, mount: Mount) {
    let Some(dir) = common::mount(test, mount) else { return };
    let t = TreeBuilder::new_in(&dir);
    let home = t.dir("home");
    let data = tempfile::tempdir().unwrap();
    let cv = t.file("home/Documents/cv.pdf", 100);
    let nm = idle_node_project(&t, "home/p");
    let mut plan = build_plan(&scan_in(&home, data.path()), &Catalog::builtin(&home), &PlanOptions::default());
    let mut forged = plan.items.iter().find(|i| i.finding.path == nm).expect("the node_modules item").clone();
    let upper = home.join("DOCUMENTS");
    forged.finding.path = upper.clone();
    plan.items = vec![forged];

    let r = clean_in(&home, data.path(), &plan);

    let item = r.items.iter().find(|i| i.path == upper).expect("the forged item");
    assert_eq!(item.outcome, Outcome::Skipped(SkipReason::Protected));
    assert!(cv.exists());
}

#[test]
fn a_protected_folder_in_another_case_is_refused_on_casefold() {
    a_protected_folder_in_another_case_is_refused_on(
        "a_protected_folder_in_another_case_is_refused_on_casefold",
        Mount::Casefold,
    );
}

#[test]
fn a_protected_folder_in_another_case_is_refused_on_exfat() {
    a_protected_folder_in_another_case_is_refused_on(
        "a_protected_folder_in_another_case_is_refused_on_exfat",
        Mount::Exfat,
    );
}

/// On a case-insensitive folder, a `.DDC-KEEP` keeps its folder: below a
/// finding's top in the scan, and at its top when made after the scan,
/// where the guard finds it through the filesystem and the walk by name.
#[test]
fn a_keep_marker_in_another_case_keeps_its_folder_on_casefold() {
    let Some(dir) = common::mount("a_keep_marker_in_another_case_keeps_its_folder_on_casefold", Mount::Casefold) else {
        return;
    };
    let t = TreeBuilder::new_in(&dir);
    let home = t.dir("home");
    let data = tempfile::tempdir().unwrap();
    let kept = idle_node_project(&t, "home/a");
    t.write("home/a/node_modules/dep/.DDC-KEEP", "");
    let later = idle_node_project(&t, "home/b");

    let scan = scan_in(&home, data.path());
    let finding = scan.findings.iter().find(|f| f.path == kept).expect("the kept finding");
    assert_eq!(finding.rule, "user.kept");
    let plan = build_plan(&scan, &Catalog::builtin(&home), &PlanOptions::default());
    t.write("home/b/node_modules/.Ddc-Keep", "");
    let r = clean_in(&home, data.path(), &plan);

    let item = r.items.iter().find(|i| i.path == later).expect("b's node_modules was planned");
    assert_eq!(item.outcome, Outcome::Skipped(SkipReason::Kept));
    assert!(later.join("dep/index.js").exists());
}
