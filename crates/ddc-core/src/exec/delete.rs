//! Deletion through a tombstone: rename first (one operation, through the
//! verified parent descriptor), then remove. An interrupted removal leaves a
//! `*.ddc-deleting-*` directory that the next scan finds as `ddc.tombstone`.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::Outcome;
use super::guard::Verified;
use crate::platform::types::HeldOpen;

/// The longest name most filesystems take, in bytes (Linux, APFS) or UTF-16
/// units (NTFS, exFAT). A shorter limit (eCryptfs) is found by the retry.
const NAME_MAX: usize = 255;

pub(super) fn remove(v: &Verified, path: &Path, make_writable: bool) -> Outcome {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or_default();
    let suffix = format!(".ddc-deleting-{stamp}-{}", std::process::id());
    if !v.unchanged() {
        return Outcome::Skipped(super::SkipReason::ChangedSinceScan);
    }
    // Safety ADR, amendment of 2026-11-04: the name is cut so the suffix
    // fits, and cut shorter when the filesystem's own limit is lower. The
    // suffix stays whole: the scan's `ddc.tombstone` rule looks for it.
    let mut keep = NAME_MAX.saturating_sub(suffix.len());
    let tomb = loop {
        let tomb = tomb_name(&v.name, &suffix, keep);
        match v.parent.rename_child(&v.name, &tomb) {
            Ok(()) => break tomb,
            Err(e) if e.kind() == io::ErrorKind::InvalidFilename && keep > 0 => keep /= 2,
            Err(e) => return rename_failed(&e),
        }
    };
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
        // FAT and exFAT on Windows: a read-only file refuses std's delete.
        let _ = crate::platform::fsops::clear_read_only(&tomb_path);
        result = remove_tree(&tomb_path);
    }
    match result {
        Ok(()) => Outcome::Deleted,
        Err(e) => Outcome::Failed { error: format!("{e} (left as {})", tomb_path.display()) },
    }
}

/// `name` cut to at most `keep` bytes, then `suffix`, cut on a character
/// boundary so the result is a valid name on APFS too. A name that is not
/// UTF-8 is spelled lossily: a tombstone needs only its suffix, the scan's
/// `ddc.tombstone` rule matches nothing else.
fn tomb_name(name: &OsStr, suffix: &str, keep: usize) -> OsString {
    let text = name.to_string_lossy();
    let mut end = keep.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    OsString::from(format!("{}{suffix}", &text[..end]))
}

/// The outcome of a refused tombstone rename: skipped held open when the
/// error carries [`HeldOpen`] (Windows, safety ADR §5, amendment of
/// 2026-11-02), failed otherwise, Linux's `EBUSY` included.
fn rename_failed(e: &io::Error) -> Outcome {
    if HeldOpen::is(e) {
        return Outcome::Skipped(super::SkipReason::HeldOpen);
    }
    Outcome::Failed { error: format!("rename to tombstone: {e}") }
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
        Verified {
            parent: DirFd::open(parent).unwrap(),
            name: name.into(),
            id: crate::platform::meta::lstat(&parent.join(name)).unwrap().id(),
            is_dir: true,
            session: None,
        }
    }

    /// Only a rename error that carries the held-open marker is a skip; a
    /// plain denial, a sharing violation and `EBUSY` stay failures.
    #[test]
    fn only_a_held_open_rename_is_skipped() {
        for held in [HeldOpen::AtOpen, HeldOpen::AtRename] {
            assert_eq!(rename_failed(&io::Error::other(held)), Outcome::Skipped(SkipReason::HeldOpen));
        }
        for code in [5, 16, 32] {
            let out = rename_failed(&io::Error::from_raw_os_error(code));
            assert!(
                matches!(&out, Outcome::Failed { error } if error.starts_with("rename to tombstone: ")),
                "{code}: {out:?}"
            );
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

    /// The tombstone's name keeps its whole suffix and cuts the name on a
    /// character boundary: never a broken UTF-8 name.
    #[test]
    fn a_long_name_is_cut_on_a_character_so_the_suffix_fits() {
        let suffix = ".ddc-deleting-1700000000000-42";
        assert_eq!(tomb_name(OsStr::new("node_modules"), suffix, 200), OsString::from(format!("node_modules{suffix}")));
        let long = "é".repeat(200); // 400 bytes
        let keep = NAME_MAX - suffix.len();
        let tomb = tomb_name(OsStr::new(&long), suffix, keep);
        let text = tomb.to_str().expect("valid UTF-8");
        assert!(text.len() <= NAME_MAX, "{}", text.len());
        assert!(text.ends_with(suffix));
        assert_eq!(text.len() - suffix.len(), keep - keep % 2, "whole two-byte characters only");
        assert_eq!(tomb_name(OsStr::new(&long), suffix, 0), OsString::from(suffix), "the floor: the suffix alone");
    }
}
