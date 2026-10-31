//! Deletion through a tombstone: rename first (one operation, through the
//! verified parent descriptor), then remove. An interrupted removal leaves a
//! `*.ddc-deleting-*` directory that the next scan finds as `ddc.tombstone`.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::Outcome;
use super::guard::Verified;

pub(super) fn remove(v: &Verified, path: &Path, make_writable: bool) -> Outcome {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or_default();
    let mut tomb = v.name.clone();
    tomb.push(OsString::from(format!(".ddc-deleting-{stamp}-{}", std::process::id())));
    if !v.unchanged() {
        return Outcome::Skipped(super::SkipReason::ChangedSinceScan);
    }
    if let Err(e) = v.parent.rename_child(&v.name, &tomb) {
        return Outcome::Failed { error: format!("rename to tombstone: {e}") };
    }
    // The rename went through the verified parent; make sure it moved the
    // directory we checked, not something swapped in meanwhile.
    match v.parent.stat_child(&tomb) {
        Ok(st) if !st.is_symlink && st.is_dir && (st.dev, st.ino) == v.id => {}
        _ => {
            let _ = v.parent.rename_child(&tomb, &v.name);
            return Outcome::Skipped(super::SkipReason::ChangedSinceScan);
        }
    }
    let tomb_path = path.with_file_name(&tomb);
    let mut result = remove_tree(&tomb_path);
    if let Err(e) = &result
        && (make_writable || e.kind() == io::ErrorKind::PermissionDenied)
    {
        make_dirs_writable(&tomb_path);
        result = remove_tree(&tomb_path);
    }
    match result {
        Ok(()) => Outcome::Deleted,
        Err(e) => Outcome::Failed { error: format!("{e} (left as {})", tomb_path.display()) },
    }
}

#[allow(clippy::disallowed_methods)] // the one place that deletes directory trees
fn remove_tree(path: &Path) -> io::Result<()> {
    // Does not follow symlinks (openat/unlinkat since Rust 1.58.1, CVE-2022-21658).
    fs::remove_dir_all(path)
}

/// Adds `u+rwx` to every real directory below `root` (Go's module cache is read-only).
fn make_dirs_writable(root: &Path) {
    let me = crate::platform::procs::uid();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(md) = fs::symlink_metadata(&dir) else { continue };
        // Another user's folder is never changed (safety ADR, amendment of 2026-10-31).
        if !md.is_dir() || crate::platform::meta::owner(&md).is_some_and(|u| u != me) {
            continue;
        }
        let mut perms = md.permissions();
        crate::platform::meta::add_owner_rwx(&mut perms);
        let _ = fs::set_permissions(&dir, perms);
        if let Ok(rd) = crate::stats::read_dir(&dir) {
            stack.extend(
                rd.filter_map(Result::ok).filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::SkipReason;
    use crate::platform::fsops::DirFd;

    fn verified(parent: &Path, name: &str) -> Verified {
        let md = fs::symlink_metadata(parent.join(name)).unwrap();
        Verified {
            parent: DirFd::open(parent).unwrap(),
            name: name.into(),
            id: crate::platform::meta::file_id(&md).unwrap(),
            is_dir: true,
            session: None,
        }
    }

    /// A folder swapped in after the check is not renamed to a tombstone or removed.
    #[test]
    fn a_folder_swapped_in_after_the_check_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        // Where the verified-parent operations (fsops) are not ported yet.
        if crate::platform::skipped_without(
            "a_folder_swapped_in_after_the_check_is_left_alone",
            crate::platform::fsops::DirFd::open(dir.path()),
        )
        .is_none()
        {
            return;
        }
        fs::create_dir_all(dir.path().join("cache/old")).unwrap();
        let v = verified(dir.path(), "cache");
        fs::rename(dir.path().join("cache"), dir.path().join("moved")).unwrap();
        fs::create_dir_all(dir.path().join("cache/new")).unwrap();

        let out = remove(&v, &dir.path().join("cache"), false);

        assert_eq!(out, Outcome::Skipped(SkipReason::ChangedSinceScan));
        assert!(dir.path().join("cache/new").is_dir());
        assert!(dir.path().join("moved/old").is_dir());
    }
}
