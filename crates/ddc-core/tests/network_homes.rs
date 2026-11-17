//! Homes on network filesystems (safety ADR, amendment of 2026-11-17;
//! story "Network and encrypted homes on Linux: NFS, CIFS, fscrypt and
//! eCryptfs", DDC-010). The NFS exports and the CIFS share come from the
//! Gate's linux-mounts job; elsewhere these tests skip, in the environment
//! group of `.config/native-skips/linux.txt`.
#![cfg(target_os = "linux")]

mod common;

use std::path::Path;
use std::time::SystemTime;

use common::{Mount, TreeBuilder};
use ddc_core::env::Env;
use ddc_core::holders::HolderCommand;
use ddc_core::model::{IdMapping, ScanReport};
use ddc_core::report::{self, ScanOptions};
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
