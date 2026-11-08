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
    let plan = build_plan(&scan, &Catalog::for_home(home), &PlanOptions::default());
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
    let catalog = Catalog::for_home(home);
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

/// A protected folder named in another case is refused: `DOCUMENTS` in a
/// plan is `Protected` as `Documents` is. The guard's case-folded path
/// check decides it before its identity check is reached, on a
/// case-sensitive folder and on a case-insensitive one alike (safety ADR,
/// amendment of 2026-11-04, §6).
fn a_protected_folder_in_another_case_is_refused_in(t: &TreeBuilder) {
    let home = t.dir("home");
    let data = tempfile::tempdir().unwrap();
    let cv = t.file("home/Documents/cv.pdf", 100);
    let nm = idle_node_project(t, "home/p");
    let mut plan = build_plan(&scan_in(&home, data.path()), &Catalog::for_home(&home), &PlanOptions::default());
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
fn a_protected_folder_in_another_case_is_refused() {
    a_protected_folder_in_another_case_is_refused_in(&TreeBuilder::new());
}

#[test]
fn a_protected_folder_in_another_case_is_refused_on_casefold() {
    let test = "a_protected_folder_in_another_case_is_refused_on_casefold";
    let Some(dir) = common::mount(test, Mount::Casefold) else { return };
    a_protected_folder_in_another_case_is_refused_in(&TreeBuilder::new_in(&dir));
}

#[test]
fn a_protected_folder_in_another_case_is_refused_on_exfat() {
    let test = "a_protected_folder_in_another_case_is_refused_on_exfat";
    let Some(dir) = common::mount(test, Mount::Exfat) else { return };
    a_protected_folder_in_another_case_is_refused_in(&TreeBuilder::new_in(&dir));
}

/// Rule names match byte for byte on a case-insensitive folder too:
/// `Node_Modules` is no `node_modules` finding, though the filesystem
/// finds the one name by the other (safety ADR, amendment of 2026-11-04,
/// §6).
fn a_rule_name_in_another_case_is_not_the_rule_in(t: &TreeBuilder) {
    let home = t.dir("home");
    let data = tempfile::tempdir().unwrap();
    let nm = idle_node_project(t, "home/p");
    for f in ["package.json", "package-lock.json"] {
        t.write(&format!("home/x/{f}"), "{}");
    }
    t.file("home/x/Node_Modules/dep/index.js", 100);

    let scan = scan_in(&home, data.path());

    let upper = home.join("x/Node_Modules");
    assert!(!scan.findings.iter().any(|f| f.path == upper), "Node_Modules is no finding: {:#?}", scan.findings);
    assert!(scan.findings.iter().any(|f| f.path == nm && f.rule == "node.node_modules"));
}

#[test]
fn a_rule_name_in_another_case_is_not_the_rule_on_casefold() {
    let test = "a_rule_name_in_another_case_is_not_the_rule_on_casefold";
    let Some(dir) = common::mount(test, Mount::Casefold) else { return };
    a_rule_name_in_another_case_is_not_the_rule_in(&TreeBuilder::new_in(&dir));
}

#[test]
fn a_rule_name_in_another_case_is_not_the_rule_on_exfat() {
    let test = "a_rule_name_in_another_case_is_not_the_rule_on_exfat";
    let Some(dir) = common::mount(test, Mount::Exfat) else { return };
    a_rule_name_in_another_case_is_not_the_rule_in(&TreeBuilder::new_in(&dir));
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
    let plan = build_plan(&scan, &Catalog::for_home(&home), &PlanOptions::default());
    t.write("home/b/node_modules/.Ddc-Keep", "");
    let r = clean_in(&home, data.path(), &plan);

    let item = r.items.iter().find(|i| i.path == later).expect("b's node_modules was planned");
    assert_eq!(item.outcome, Outcome::Skipped(SkipReason::Kept));
    assert!(later.join("dep/index.js").exists());
}

/// Findings whose `node_modules` hold a flag only root sets or clears (a
/// `chattr +i` file, a `chattr +a` file, a `chattr +i` folder) or carry
/// one itself: a plan made blind to the flags, as one made before they
/// were set, is refused by the clean's own walk and stat; the fresh scan
/// lists each `immutable`; and `--items` refuses each again before any
/// rename, while the project without a flag goes (safety ADR, amendment of
/// 2026-11-04, "Linux file kinds").
#[test]
fn findings_holding_a_flag_only_root_sets_are_listed_and_refused_through_items() {
    use ddc_core::model::{NotSelected, RemovalKind};
    use ddc_core::plan::{ActionKind, ListedItem, plan_listed};
    use ddc_core::report::recheck;
    use ddc_core::report::volumes::SystemStatFs;
    use ddc_core::scan::ReaderKind;
    let test = "findings_holding_a_flag_only_root_sets_are_listed_and_refused_through_items";
    let Some(home) = common::prepared(test, Prepared::ImmutableTree) else {
        return;
    };
    let data = tempfile::tempdir().unwrap();
    let env = Env::with_paths(&home, data.path(), &data.path().join("logs"), SystemTime::now()).unwrap();
    let catalog = Catalog::for_home(&home);
    let locked = ["p1", "p2", "p3", "p4"].map(|p| home.join(p).join("node_modules"));
    let plain = home.join("p5/node_modules");
    let refused = Outcome::Skipped(SkipReason::Immutable);

    // The reference reader tells no flag on Linux: a scan blind to them.
    let walk = WalkOptions { threads: Some(2), reader: ReaderKind::Std, ..Default::default() };
    let stale = report::scan(&env, &ScanOptions { walk, ..Default::default() }, &Progress::default()).unwrap();
    let mut plan = build_plan(&stale, &catalog, &PlanOptions::default());
    plan.items.retain(|i| locked.contains(&i.finding.path));
    assert_eq!(plan.items.iter().filter(|i| i.selected).count(), 4, "planned blind: {:#?}", plan.items);
    let r = clean_in(&home, data.path(), &plan);
    for path in &locked {
        let item = r.items.iter().find(|i| &i.path == path).expect("planned");
        assert_eq!(item.outcome, refused, "{path:?}");
    }

    let scan = scan_in(&home, data.path());
    let listed: Vec<ListedItem> = locked
        .iter()
        .chain([&plain])
        .map(|path| {
            let f = scan.findings.iter().find(|f| &f.path == path).expect("a finding");
            assert_eq!(f.not_selected == Some(NotSelected::Immutable), path != &plain, "{path:?}");
            ListedItem {
                path: f.path.clone(),
                dev: f.dev,
                ino: f.ino,
                rule: f.rule.clone(),
                class: f.class,
                action: if f.removal == RemovalKind::Trash { ActionKind::Trash } else { ActionKind::Delete },
                permanent: false,
            }
        })
        .collect();
    let opts = ScanOptions { walk: WalkOptions { threads: Some(2), ..Default::default() }, ..Default::default() };
    let (rechecked, entries) =
        recheck::pruned(&env, &opts, &catalog, &listed, &Progress::default(), &SystemStatFs, None).unwrap();
    let items = plan_listed(&rechecked, &entries, &catalog, &listed);
    let mut journal = MemoryJournal::default();
    let ports = Ports {
        commands: &FakeCommands::default(),
        trash: &CrossingTrash,
        procs: &FakeProcs::new(vec![]),
        journal: &mut journal,
    };
    let r = exec::execute(&env, &catalog, &items.plan, &Consent::Listed, ports, &ExecOptions::default()).unwrap();

    for path in &locked {
        let item = r.items.iter().find(|i| &i.path == path).expect("listed");
        assert_eq!(item.outcome, refused, "{path:?}");
        let tombstones: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n.to_string_lossy().contains(".ddc-deleting-"))
            .collect();
        assert!(tombstones.is_empty(), "{path:?}: {tombstones:?}");
        assert!(path.join("dep/index.js").exists());
    }
    let item = r.items.iter().find(|i| i.path == plain).expect("listed");
    assert_eq!(item.outcome, Outcome::Deleted);
}

/// A tombstone that another filesystem was mounted into after the guard's
/// check (a tmpfs, a bind mount, a btrfs subvolume of the user's): the
/// removal through descriptors stops there with an error, and the mount,
/// the bound folder and the subvolume keep what they hold (safety ADR §5,
/// amendment of 2026-11-04, "removal never crosses a mount").
#[test]
fn a_removal_never_crosses_a_mount_made_inside_a_tombstone() {
    use ddc_core::platform::fsops::DirFd;
    let test = "a_removal_never_crosses_a_mount_made_inside_a_tombstone";
    let (Some(mounts), Some(btrfs)) =
        (common::prepared(test, Prepared::TombMounts), common::prepared(test, Prepared::TombSubvolume))
    else {
        return;
    };
    for (parent, name, keep) in [
        (&mounts, "tmpfs.ddc-deleting-1-1", "mnt/keep"),
        (&mounts, "bind.ddc-deleting-1-1", "mnt/keep"),
        (&btrfs, "subvolume.ddc-deleting-1-1", "sub/keep"),
    ] {
        let tomb = parent.join(name);
        let dir = DirFd::open(parent).unwrap();
        let st = dir.stat_child(name.as_ref()).unwrap();
        let err = dir.remove_tree(name.as_ref(), &tomb, (st.dev, st.ino)).unwrap_err();
        assert!(err.to_string().contains("another filesystem"), "{name}: {err}");
        assert!(tomb.join(keep).exists(), "{name}: what lies beyond was removed");
    }
    assert!(mounts.join("bind-source/keep").exists());
}
