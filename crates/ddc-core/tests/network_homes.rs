//! Homes on network filesystems (safety ADR, amendment of 2026-11-17;
//! story "Network and encrypted homes on Linux: NFS, CIFS, fscrypt and
//! eCryptfs", DDC-010). The NFS exports and the CIFS share come from the
//! Gate's linux-mounts job; elsewhere these tests skip, in the environment
//! group of `.config/native-skips/linux.txt`.
#![cfg(target_os = "linux")]

mod common;

use std::path::Path;
use std::time::SystemTime;

use common::fakes::{CrossingTrash, FakeCommands, FakeProcs};
use common::{Mount, TreeBuilder};
use ddc_core::class::RiskClass;
use ddc_core::env::Env;
use ddc_core::exec::{
    self, Consent, ExecOptions, ExecReport, MemoryJournal, Outcome, Ports, ProcessTable, SkipReason, SystemProcesses,
};
use ddc_core::holders::HolderCommand;
use ddc_core::model::{IdMapping, ScanReport};
use ddc_core::plan::{PlanOptions, build_plan};
use ddc_core::report::{self, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::scan::{Progress, WalkOptions};
use ddc_core::store::{self, LockMode};

/// A scan of `home` with its data dir at `data`, holding the lock as
/// `ddc scan` does, so this run's holder record lies in `data`.
fn scan_with_data(home: &Path, data: &Path) -> ScanReport {
    let env = Env::with_paths(home, data, &data.join("logs"), SystemTime::now()).unwrap();
    let lock = store::try_lock(&env, LockMode::Shared, HolderCommand::Scan).unwrap();
    assert!(lock.record().is_some(), "the holder record was written");
    let walk = WalkOptions { threads: Some(2), ..WalkOptions::default() };
    let opts = ScanOptions { walk, holder_record: lock.record(), ..ScanOptions::default() };
    report::scan(&env, &opts, &Progress::default()).unwrap()
}

fn mappings(r: &ScanReport) -> Vec<&IdMapping> {
    r.volumes.iter().filter_map(|v| v.id_mapping.as_ref()).collect()
}

/// On the `all_squash` export a file the run makes belongs to nobody, so a
/// home there is reported once as mapped to 65534, with the data dir on the
/// export; with the data dir on local disk nothing is mapped.
#[test]
fn a_squashed_nfs_home_is_reported_once_as_mapped() {
    let Some(dir) = common::mount("a_squashed_nfs_home_is_reported_once_as_mapped", Mount::NfsSquash) else {
        return;
    };
    let tree = TreeBuilder::new_in(&dir);
    tree.file("Documents/a.txt", 100);
    tree.file(".cache/pip/http/b", 100);

    let r = scan_with_data(tree.root(), &tree.path(".local/share/ddc"));
    let m = mappings(&r);
    assert_eq!(m.len(), 1, "{:?}", r.volumes);
    assert_eq!((m[0].owner, &m[0].roots, m[0].filesystem.as_str()), (65534, &r.scan_roots, "nfs4"));
    assert!(m[0].entries >= 4, "the walk counted the squashed entries: {}", m[0].entries);
    let foreign = Some(ddc_core::model::NotSelected::ForeignOwner);
    assert!(r.findings.iter().all(|f| f.not_selected != foreign), "no foreign-owner line: {:?}", r.findings);

    let local = tempfile::tempdir().unwrap();
    let r = scan_with_data(tree.root(), local.path());
    assert!(mappings(&r).is_empty(), "the data dir on another mount: {:?}", r.volumes);
}

/// On the plain NFS export and on the CIFS share mounted with `uid=`, the
/// run's own file is the user's, so nothing is mapped.
#[test]
fn a_plain_nfs_or_cifs_home_is_not_mapped() {
    for kind in [Mount::Nfs, Mount::Cifs] {
        let Some(dir) = common::mount("a_plain_nfs_or_cifs_home_is_not_mapped", kind) else { continue };
        let tree = TreeBuilder::new_in(&dir);
        tree.file("Documents/a.txt", 100);
        let r = scan_with_data(tree.root(), &tree.path(".local/share/ddc"));
        assert!(mappings(&r).is_empty(), "{kind:?}: {:?}", r.volumes);
    }
}

/// On the `all_squash` export the user's cache is the squashed uid's, and
/// so are its read-only folders (Go's module cache is one): a clean of this
/// run, mapped by its own probe, plans the cache and removes it whole, its
/// 0555 folder made writable on its way out, and leaves no tombstone
/// (safety ADR, amendment of 2026-11-17, "M's entries count as the user's").
#[test]
fn a_squashed_home_clean_removes_a_read_only_cache_whole() {
    let Some(dir) = common::mount("a_squashed_home_clean_removes_a_read_only_cache_whole", Mount::NfsSquash) else {
        return;
    };
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    let tree = TreeBuilder::new_in(&dir);
    tree.file(".cache/pip/http/a", 1000);
    tree.file(".cache/pip/ro/b", 1000);
    let (home, cache, ro) = (tree.root().to_path_buf(), tree.path(".cache/pip"), tree.path(".cache/pip/ro"));
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
    assert_eq!(std::fs::metadata(&ro).unwrap().uid(), 65534);

    let data = tree.path(".local/share/ddc");
    let env = Env::with_paths(&home, &data, &data.join("logs"), SystemTime::now()).unwrap();
    let lock = store::try_lock(&env, LockMode::Exclusive, HolderCommand::Clean).unwrap();
    let walk = WalkOptions { threads: Some(2), ..WalkOptions::default() };
    let opts = ScanOptions { walk, holder_record: lock.record(), ..ScanOptions::default() };
    let scan = report::scan(&env, &opts, &Progress::default()).unwrap();
    assert_eq!(scan.mapped_owner(), Some(65534));
    let f = scan.findings.iter().find(|f| f.path == cache).expect("the cache is a finding");
    assert!(f.selected && f.not_selected.is_none(), "{f:?}");

    let catalog = Catalog::for_home(&home);
    let plan = build_plan(&scan, &catalog, &PlanOptions::default());
    assert!(plan.items.iter().any(|i| i.finding.path == cache), "planned");
    let mut journal = MemoryJournal::default();
    let consent = Consent::AssumeYes { classes: vec![RiskClass::Safe, RiskClass::Rebuildable] };
    let ports = Ports {
        commands: &FakeCommands::default(),
        trash: &CrossingTrash,
        procs: &FakeProcs::new(vec![]),
        journal: &mut journal,
    };
    let mapped_roots = scan.mapped_roots();
    let exec_opts =
        ExecOptions { mapped_owner: scan.mapped_owner(), mapped_roots: &mapped_roots, ..ExecOptions::default() };
    let r = exec::execute(&env, &catalog, &plan, &consent, ports, &exec_opts).unwrap();
    let item = r.items.iter().find(|i| i.path == cache).expect("the cache's outcome");
    assert_eq!(item.outcome, Outcome::Deleted, "{item:?}");
    assert!(!cache.exists(), "removed whole");
    let left: Vec<_> = std::fs::read_dir(tree.path(".cache")).unwrap().flatten().map(|e| e.file_name()).collect();
    assert!(left.iter().all(|n| !n.to_string_lossy().contains(".ddc-deleting-")), "no tombstone: {left:?}");
}

/// On the `all_squash` export the Trash stays refused: a `.Trash-$uid` made
/// through it is nobody's, so not the user's. Where none exists yet the dry
/// run cannot tell and predicts a move; the move makes it, sees it nobody's
/// and refuses, and nothing moves; then the dry run refuses too (safety
/// ADR, amendment of 2026-11-17, "The Trash on a mapped root does not
/// change").
#[test]
fn a_squashed_exports_trash_is_refused_and_nothing_moves() {
    use ddc_core::platform::trash;
    use ddc_core::platform::types::{TrashAsk, TrashError};
    let Some(top) = common::mount("a_squashed_exports_trash_is_refused_and_nothing_moves", Mount::NfsSquash) else {
        return;
    };
    let tree = TreeBuilder::new_in(&top);
    let file = tree.file("cache/blob", 4096);
    let id = ddc_core::platform::meta::lstat(&file).unwrap().id();
    let folder = ddc_core::platform::fsops::DirFd::open(file.parent().unwrap()).unwrap();
    let name = file.file_name().unwrap();
    let ask = [TrashAsk { name, path: &file, bytes: 0, is_dir: false }];
    let admit = || trash::admit(&mut trash::Budget, &folder, &ask);

    let made = top.join(format!(".Trash-{}", ddc_core::platform::procs::uid())).exists();
    if !made {
        assert!(admit().is_ok(), "no Trash yet: the dry run predicts a move");
    }
    assert!(matches!(trash::trash_item(&folder, name, &file, id), Err(TrashError::CrossesFilesystem)));
    assert!(file.exists(), "nothing moved");
    assert!(matches!(admit(), Err(TrashError::CrossesFilesystem)), "the Trash is nobody's now: refused");
}

/// A child that holds `file` open on its standard input, killed on drop.
struct Holder(std::process::Child);

impl Holder {
    fn of(file: &Path) -> Self {
        let child =
            std::process::Command::new("sleep").arg("600").stdin(std::fs::File::open(file).unwrap()).spawn().unwrap();
        let pid = child.id() as i32;
        let holder = Self(child);
        // Until the in-use check can see its descriptor.
        for _ in 0..100 {
            if SystemProcesses.open_files(pid).iter().any(|f| f.file_name() == file.file_name()) {
                return holder;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("the holder's open file never showed");
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// On the plain NFS export a file another process holds open in a selected
/// cache (safety ADR §5, amendment of 2026-11-17, "NFS silly renames"): the
/// in-use check refuses the cache; a clean that cannot see the holder
/// deletes it, and the NFS client's silly rename stays in the tombstone,
/// named by `left_open` on the item and its journal line; a rescan while it
/// is open has no finding there; once it is closed the tombstone is a safe
/// finding again, and its clean leaves nothing.
#[test]
fn a_file_held_open_on_nfs_is_left_open_in_its_tombstone_and_goes_once_closed() {
    let name = "a_file_held_open_on_nfs_is_left_open_in_its_tombstone_and_goes_once_closed";
    let Some(dir) = common::mount(name, Mount::Nfs) else {
        return;
    };
    let tree = TreeBuilder::new_in(&dir);
    tree.file(".cache/pip/http/a", 1000);
    let open = tree.file(".cache/pip/http/b", 1000);
    let (home, cache) = (tree.root().to_path_buf(), tree.path(".cache/pip"));
    let data = tree.path(".local/share/ddc");
    let env = Env::with_paths(&home, &data, &data.join("logs"), SystemTime::now()).unwrap();
    let catalog = Catalog::for_home(&home);
    let scan = || {
        let walk = WalkOptions { threads: Some(2), ..WalkOptions::default() };
        report::scan(&env, &ScanOptions { walk, ..ScanOptions::default() }, &Progress::default()).unwrap()
    };
    let clean = |procs: &dyn ProcessTable, path: &Path| -> (ExecReport, MemoryJournal) {
        let plan = build_plan(&scan(), &catalog, &PlanOptions::default());
        assert!(plan.items.iter().any(|i| i.finding.path == path && i.selected), "{path:?} planned");
        let mut journal = MemoryJournal::default();
        let consent = Consent::AssumeYes { classes: vec![RiskClass::Safe, RiskClass::Rebuildable] };
        let ports = Ports { commands: &FakeCommands::default(), trash: &CrossingTrash, procs, journal: &mut journal };
        let r = exec::execute(&env, &catalog, &plan, &consent, ports, &ExecOptions::default()).unwrap();
        (r, journal)
    };
    let item = |r: &ExecReport, path: &Path| r.items.iter().find(|i| i.path == path).cloned().expect("its outcome");
    let tombs = || -> Vec<std::path::PathBuf> {
        std::fs::read_dir(tree.path(".cache"))
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().contains(".ddc-deleting-"))
            .collect()
    };

    let holder = Holder::of(&open);
    let (r, _) = clean(&SystemProcesses, &cache);
    let refused = item(&r, &cache);
    assert!(
        matches!(&refused.outcome, Outcome::Skipped(SkipReason::InUse { pid, .. }) if *pid == holder.0.id() as i32),
        "{refused:?}"
    );
    assert!(open.exists());

    let (r, journal) = clean(&FakeProcs::new(vec![]), &cache);
    let done = item(&r, &cache);
    assert_eq!(done.outcome, Outcome::Deleted, "{done:?}");
    assert_eq!(done.left_open.len(), 1, "{done:?}");
    let silly = &done.left_open[0];
    let tomb = tombs();
    assert_eq!(tomb.len(), 1, "one tombstone: {tomb:?}");
    assert!(silly.starts_with(&tomb[0]), "{silly:?} in {tomb:?}");
    assert!(ddc_core::platform::types::is_silly_rename(silly.file_name().unwrap()), "{silly:?}");
    assert!(silly.exists(), "the client keeps it while it is open");
    let line = journal.0.iter().find(|l| l.path == cache).expect("its journal line");
    assert_eq!(&line.left_open, &done.left_open);
    assert!(!cache.exists());

    let r = scan();
    assert!(r.findings.iter().all(|f| !f.path.starts_with(&tomb[0])), "nothing to offer: {:?}", r.findings);

    drop(holder);
    for _ in 0..250 {
        if !silly.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!silly.exists(), "the client removes it once it is closed");
    let (r, _) = clean(&FakeProcs::new(vec![]), &tomb[0]);
    let done = item(&r, &tomb[0]);
    assert_eq!((&done.outcome, done.left_open.len()), (&Outcome::Deleted, 0), "{done:?}");
    assert!(tombs().is_empty(), "nothing left");
}
