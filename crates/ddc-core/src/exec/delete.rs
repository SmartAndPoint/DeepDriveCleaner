//! Deletion through a tombstone: rename first (one operation, through the
//! verified parent descriptor), then remove through descriptors, never by
//! path. An interrupted or stopped removal leaves a `*.ddc-deleting-*`
//! directory that the next scan finds as `ddc.tombstone`.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use super::Outcome;
use super::guard::Verified;
use crate::platform::types::HeldOpen;

/// The longest name most filesystems take, in bytes (Linux, APFS) or UTF-16
/// units (NTFS, exFAT). A shorter limit (eCryptfs) is found by the retry.
const NAME_MAX: usize = 255;

/// Removes the verified folder. A read-only tree (Go's module cache) needs
/// no flag of its own: on macOS and Linux every folder of the user's own
/// inside is made writable on its way out, on Windows each folder's
/// read-only attribute is cleared when the first try fails.
pub(super) fn remove(v: &Verified, path: &Path) -> Outcome {
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or_default();
    let suffix = format!(".ddc-deleting-{stamp}-{}", std::process::id());
    if !v.unchanged() {
        return Outcome::Skipped(super::SkipReason::ChangedSinceScan);
    }
    // Safety ADR, amendment of 2026-11-04: the name is cut so the suffix
    // fits, and cut shorter when the filesystem's own limit is lower. The
    // suffix stays whole: the scan's `ddc.tombstone` rule looks for it.
    let tomb = match rename_to_tomb(&v.name, &suffix, |tomb| v.parent.rename_child(&v.name, tomb)) {
        Ok(tomb) => tomb,
        Err(e) => return rename_failed(&e),
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
    // Through the verified parent's descriptor, never crossing a mount, a
    // bind mount or a btrfs subvolume made inside since the guard's check
    // (safety ADR §5, amendment of 2026-11-04). A removal that stops keeps
    // its tombstone, never renamed back.
    match v.parent.remove_tree(&tomb, &tomb_path, v.id) {
        Ok(()) => Outcome::Deleted,
        Err(e) => Outcome::Failed { error: format!("{e} (left as {})", tomb_path.display()) },
    }
}

/// Renames to a tombstone through `rename`: first `name` cut to fit
/// [`NAME_MAX`] with `suffix`, then, while the filesystem says the name is
/// too long, with half as much of `name` as the last try used, down to the
/// suffix alone. Each try is shorter than the one before.
fn rename_to_tomb(
    name: &OsStr,
    suffix: &str,
    mut rename: impl FnMut(&OsStr) -> io::Result<()>,
) -> io::Result<OsString> {
    let mut keep = NAME_MAX.saturating_sub(suffix.len());
    loop {
        let tomb = tomb_name(name, suffix, keep);
        match rename(&tomb) {
            Ok(()) => return Ok(tomb),
            Err(e) if e.kind() == io::ErrorKind::InvalidFilename => {
                let used = tomb.len() - suffix.len();
                if used == 0 {
                    return Err(e);
                }
                keep = used / 2;
            }
            Err(e) => return Err(e),
        }
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

#[cfg(test)]
mod tests {
    use std::fs;

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

        let out = remove(&v, &dir.path().join("cache"));

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

    /// While the filesystem says the name is too long, each try is shorter
    /// than the last, down to the suffix alone; then the rename fails. A
    /// short name is not tried twice.
    #[test]
    fn a_name_too_long_for_the_filesystem_is_tried_shorter_each_time() {
        let suffix = ".ddc-deleting-1-1";
        let too_long = || io::Error::from(io::ErrorKind::InvalidFilename);
        let tries = |name: &str, limit: usize| {
            let mut seen = Vec::new();
            let out = rename_to_tomb(OsStr::new(name), suffix, |t| {
                seen.push(t.len());
                if t.len() > limit { Err(too_long()) } else { Ok(()) }
            });
            (out.map(|t| t.len()), seen)
        };
        // eCryptfs-like: 143 bytes.
        let (out, seen) = tries(&"a".repeat(255), 143);
        assert!(out.as_ref().is_ok_and(|n| *n <= 143), "{out:?}");
        assert!(seen.windows(2).all(|w| w[1] < w[0]), "shorter each time: {seen:?}");
        // A short name the filesystem refuses whole: one try with it, then
        // shorter, then the suffix alone, then the failure.
        let (out, seen) = tries("node_modules", 0);
        assert_eq!(out.unwrap_err().kind(), io::ErrorKind::InvalidFilename);
        assert_eq!(seen.first(), Some(&("node_modules".len() + suffix.len())));
        assert_eq!(seen.last(), Some(&suffix.len()));
        assert!(seen.windows(2).all(|w| w[1] < w[0]), "never the same try twice: {seen:?}");
        // Another error is not retried.
        let mut calls = 0;
        let out = rename_to_tomb(OsStr::new("x"), suffix, |_| {
            calls += 1;
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        });
        assert_eq!((out.unwrap_err().kind(), calls), (io::ErrorKind::PermissionDenied, 1));
    }
}
