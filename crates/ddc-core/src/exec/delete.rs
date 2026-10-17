//! Deletion through a tombstone: rename first (one operation, through the
//! verified parent descriptor), then remove. An interrupted removal leaves a
//! `*.ddc-deleting-*` directory that the next scan finds as `ddc.tombstone`.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt as _;
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
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(md) = fs::symlink_metadata(&dir) else { continue };
        if !md.is_dir() {
            continue;
        }
        let mut perms = md.permissions();
        perms.set_mode(perms.mode() | 0o700);
        let _ = fs::set_permissions(&dir, perms);
        if let Ok(rd) = fs::read_dir(&dir) {
            stack.extend(
                rd.filter_map(Result::ok).filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()),
            );
        }
    }
}
