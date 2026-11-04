//! Canonical paths and the key paths are compared by, on Windows (story
//! "Windows scanner and rule catalog", DDC-007, step 2).
//!
//! [`canonicalize`] opens the path, as std's does, and asks for its final
//! name (`GetFinalPathNameByHandleW`), which resolves links, 8.3 short names
//! and the case as stored, but returns the verbatim form `\\?\C:\…`. Reports and
//! comparisons use the plain form `C:\…`; std adds the verbatim prefix again
//! by itself where a call needs it (a path over `MAX_PATH`). A path whose
//! names the plain form cannot spell (a trailing dot or space, a reserved
//! device name) keeps the prefix: it then never equals a scanned path and
//! fails closed at the identity check.
//!
//! The key folds what Windows treats as the same name: both separators, and
//! the case of each UTF-16 unit (NTFS compares names case-insensitively by
//! default). Catalog paths are written with `/`, the disk gives `\`.

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Component, Path, PathBuf, Prefix};

use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_NO_RECALL, FILE_NAME_NORMALIZED, GetDriveTypeW,
    GetFinalPathNameByHandleW, VOLUME_NAME_DOS,
};

/// `GetDriveTypeW`'s answers that are not a local drive (WinBase.h; their
/// windows-sys module is not otherwise needed).
const DRIVE_UNKNOWN: u32 = 0;
const DRIVE_REMOTE: u32 = 4;

/// The final path of `path`, without the verbatim prefix where the plain
/// form spells the same names. What std's canonicalize does (an open with
/// no access, then `GetFinalPathNameByHandleW`), with the open told never to
/// recall a cloud placeholder (`FILE_FLAG_OPEN_NO_RECALL`): canonicalising a
/// folder that is not downloaded never downloads it. Links are followed.
/// std after 1.99 adds a fallback for volumes the mount manager does not
/// know (some RAM disks and FUSE drivers), where this answers an error.
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle as _;
    let file = std::fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_NO_RECALL)
        .open(path)?;
    let mut buffer = vec![0u16; 512];
    loop {
        let capacity = u32::try_from(buffer.len()).map_err(|_| io::Error::from_raw_os_error(206))?;
        // SAFETY: the handle is open for the call; the buffer is writable for
        // `capacity` units.
        let n = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                capacity,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        let n = usize::try_from(n).expect("u32 fits usize");
        if n == 0 {
            return Err(io::Error::last_os_error());
        }
        if n < buffer.len() {
            buffer.truncate(n);
            return Ok(simplified(PathBuf::from(OsString::from_wide(&buffer))));
        }
        // Too small: `n` is the size needed, with its NUL; always grow.
        buffer.resize(n.max(buffer.len() + 1), 0);
    }
}

/// `path` with `\\?\C:\` turned into `C:\` and `\\?\UNC\server\share` into
/// `\\server\share`, when every name can be spelled plainly.
pub(super) fn simplified(path: PathBuf) -> PathBuf {
    let mut components = path.components();
    let plain = match components.next() {
        Some(Component::Prefix(p)) => match p.kind() {
            Prefix::VerbatimDisk(drive) => {
                let mut s = OsString::from(char::from(drive).to_string());
                s.push(":");
                s
            }
            Prefix::VerbatimUNC(server, share) => {
                let mut s = OsString::from(r"\\");
                s.push(server);
                s.push(r"\");
                s.push(share);
                s
            }
            _ => return path,
        },
        _ => return path,
    };
    let rest: Vec<Component<'_>> = components.collect();
    let spellable = rest.iter().all(|c| match c {
        Component::RootDir => true,
        Component::Normal(name) => plainly_spelled(name),
        _ => false,
    });
    if !spellable {
        return path;
    }
    let mut out = PathBuf::from(plain);
    out.push(r"\");
    for c in rest.into_iter().filter(|c| matches!(c, Component::Normal(_))) {
        out.push(c.as_os_str());
    }
    out
}

/// A name the plain (Win32) form keeps as it is: no trailing dot or space,
/// which Win32 strips, and no device name such as `NUL` or `com1.txt`.
pub(super) fn plainly_spelled(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else { return true };
    if name.ends_with('.') || name.ends_with(' ') {
        return false;
    }
    let stem = name.split('.').next().unwrap_or(name).trim_end().to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0');
    !device
}

/// `path` with `\` as its only separator and each UTF-16 unit upper-cased
/// where it has a single upper-case form in the Basic Multilingual Plane.
pub fn key(path: &Path) -> Cow<'_, Path> {
    let mut out = PathBuf::new();
    for c in path.components() {
        out.push(upper(c.as_os_str()));
    }
    Cow::Owned(out)
}

fn upper(name: &OsStr) -> OsString {
    let wide: Vec<u16> = name
        .encode_wide()
        .map(|u| {
            let Some(c) = char::from_u32(u32::from(u)) else { return u };
            let mut up = c.to_uppercase();
            match (up.next(), up.next()) {
                (Some(one), None) => u16::try_from(u32::from(one)).unwrap_or(u),
                _ => u,
            }
        })
        .collect();
    OsString::from_wide(&wide)
}

/// Whether `path`, read from a file's content (a `gitdir:` line, a
/// `CMakeCache.txt`), may be opened: a path on a local drive. A share on
/// another machine (`\\server\share`, `\\?\UNC\…`), a device path, a path
/// with no drive and a drive Windows calls remote (`net use`, or a `subst`
/// of a share) are not. A drive letter with nothing behind it is local: its
/// open fails at once, and an unplugged disk's worktree is still an orphan.
/// The path as written is checked; callers check a canonical path again,
/// since a local symlink may lead to a share. Otherwise: opening one connects to that machine, offers the
/// user's credentials there, and can hold a scan thread for the network's
/// timeout when the host does not answer.
pub fn is_local(path: &Path) -> bool {
    let drive = match path.components().next() {
        Some(Component::Prefix(p)) => match p.kind() {
            Prefix::Disk(d) | Prefix::VerbatimDisk(d) => d,
            _ => return false,
        },
        _ => return false,
    };
    let root: Vec<u16> = format!("{}:\\", char::from(drive)).encode_utf16().chain([0]).collect();
    // SAFETY: a NUL-terminated wide root; the call only reads it.
    let kind = unsafe { GetDriveTypeW(root.as_ptr()) };
    !matches!(kind, DRIVE_REMOTE | DRIVE_UNKNOWN)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A drive letter with nothing assigned, from the system's own list.
    fn free_drive() -> char {
        // SAFETY: no arguments; returns a bitmask of the assigned letters.
        let used = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
        (b'D'..=b'Z').rev().find(|d| used & (1 << (d - b'A')) == 0).map(char::from).expect("a free drive letter")
    }

    #[test]
    fn an_unplugged_disk_s_worktree_is_still_an_orphan() {
        let d = tempfile::tempdir().unwrap();
        let wt = d.path().join("wt");
        std::fs::create_dir(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}:/repo/.git/worktrees/wt\n", free_drive())).unwrap();
        assert!(matches!(
            crate::rules::worktree::recognize(&wt),
            Some(crate::rules::worktree::Recognized::Orphan { .. })
        ));
    }

    #[test]
    fn a_path_on_another_machine_is_never_followed() {
        let d = tempfile::tempdir().unwrap();
        let wt = d.path().join("wt");
        std::fs::create_dir(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: \\\\server\\share\\repo\\.git\\worktrees\\wt\n").unwrap();
        assert_eq!(crate::report::project::git_dir(&wt), None, "the .git file's line");
        assert!(crate::rules::worktree::recognize(&wt).is_none());

        // A local gitdir whose commondir names a share.
        let gitdir = d.path().join("repo/.git/worktrees/wt");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("commondir"), "\\\\server\\share\\repo\\.git\n").unwrap();
        std::fs::write(gitdir.join("gitdir"), format!("{}\n", wt.join(".git").display())).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", gitdir.display())).unwrap();
        assert!(crate::rules::worktree::recognize(&wt).is_none(), "commondir on another machine");
        std::fs::write(gitdir.join("commondir"), "../..\n").unwrap();
        assert!(
            matches!(crate::rules::worktree::recognize(&wt), Some(crate::rules::worktree::Recognized::Linked(_))),
            "the same worktree, all local"
        );
    }

    #[test]
    fn only_a_local_drive_is_local() {
        let system = std::env::temp_dir();
        assert!(is_local(&system), "{}", system.display());
        assert!(is_local(Path::new(r"\\?\C:\Windows")));
        assert!(!is_local(Path::new(r"\\server\share\repo\.git")));
        assert!(!is_local(Path::new(r"\\?\UNC\server\share\x")));
        assert!(!is_local(Path::new(r"\\.\pipe\x")));
        assert!(!is_local(Path::new(r"\Windows")), "no drive");
        assert!(!is_local(Path::new("relative")));
        assert!(!is_local(Path::new("//server/share/repo/.git")), "git writes forward slashes");
        assert!(!is_local(Path::new(r"\\.\C:\x")), "a device path, even to a drive");
        assert!(
            is_local(Path::new(&format!("{}:\\x", free_drive()))),
            "nothing mounted: local, the open fails at once"
        );
    }

    #[test]
    fn the_verbatim_prefix_goes_where_the_names_are_spellable() {
        let s = |p: &str| simplified(PathBuf::from(p));
        assert_eq!(s(r"\\?\C:\Users\u\AppData"), PathBuf::from(r"C:\Users\u\AppData"));
        assert_eq!(s(r"\\?\C:\"), PathBuf::from(r"C:\"));
        assert_eq!(s(r"\\?\UNC\srv\share\dir"), PathBuf::from(r"\\srv\share\dir"));
        assert_eq!(s(r"C:\already\plain"), PathBuf::from(r"C:\already\plain"));
        assert_eq!(s(r"\\?\C:\dir\name."), PathBuf::from(r"\\?\C:\dir\name."), "a trailing dot");
        assert_eq!(s(r"\\?\C:\dir\name "), PathBuf::from(r"\\?\C:\dir\name "), "a trailing space");
        assert_eq!(s(r"\\?\C:\dir\nul.txt"), PathBuf::from(r"\\?\C:\dir\nul.txt"), "a device name");
        assert_eq!(s(r"\\?\C:\dir\COM1"), PathBuf::from(r"\\?\C:\dir\COM1"));
        assert_eq!(s(r"\\?\C:\dir\COM0"), PathBuf::from(r"C:\dir\COM0"), "COM0 is no device");
        assert_eq!(s(r"\\?\C:\dir\console"), PathBuf::from(r"C:\dir\console"));
    }

    #[test]
    fn a_key_folds_case_and_separators() {
        assert_eq!(
            key(Path::new("C:/Users/u/AppData/Local/Microsoft/Windows/INetCookies")),
            key(Path::new(r"c:\users\U\appdata\local\microsoft\windows\InetCookies"))
        );
        assert_eq!(key(Path::new(r"C:\Пользователи\Ä")), key(Path::new(r"c:\пользователи\ä")));
        assert_ne!(key(Path::new(r"C:\a\b")), key(Path::new(r"C:\a\bc")));
        // A unit with no single upper-case form stays as it is.
        assert_eq!(key(Path::new("C:\\ß")), Cow::<Path>::Owned(PathBuf::from("C:\\ß")));
    }

    /// A real folder: the final path drops the prefix, resolves the case
    /// and resolves an 8.3 name where the volume makes them.
    #[test]
    fn a_real_folder_comes_back_plain_in_its_stored_case() {
        let d = tempfile::tempdir().unwrap();
        let named = d.path().join("Mixed Case Folder");
        std::fs::create_dir(&named).unwrap();
        let canonical = canonicalize(&d.path().join("MIXED CASE FOLDER")).unwrap();
        assert!(!canonical.as_os_str().to_string_lossy().starts_with(r"\\?\"), "{}", canonical.display());
        assert_eq!(canonical.file_name().unwrap(), "Mixed Case Folder");
    }

    /// The no-recall open gives what std's canonicalize gives: a folder, a
    /// file, a junction (resolved to its target), a path past `MAX_PATH`,
    /// and a missing path's error.
    #[test]
    #[allow(clippy::disallowed_methods)] // std's canonicalize is the reference here
    fn the_final_path_is_std_s() {
        let d = tempfile::tempdir().unwrap();
        let folder = d.path().join("Folder");
        std::fs::create_dir(&folder).unwrap();
        std::fs::write(folder.join("file.txt"), b"x").unwrap();
        let link = d.path().join("link");
        let made = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(&link)
            .arg(&folder)
            .output()
            .unwrap();
        assert!(made.status.success());
        let mut deep = folder.clone();
        for i in 0..12 {
            deep.push(format!("a-rather-long-folder-name-{i:02}"));
        }
        std::fs::create_dir_all(&deep).unwrap();
        for p in [d.path().to_path_buf(), folder.clone(), folder.join("file.txt"), link.join("file.txt"), deep.clone()]
        {
            assert_eq!(canonicalize(&p).unwrap(), simplified(std::fs::canonicalize(&p).unwrap()), "{}", p.display());
        }
        assert!(deep.as_os_str().len() > 260);
        assert_eq!(canonicalize(&link).unwrap(), canonicalize(&folder).unwrap(), "a junction resolves");
        let missing = d.path().join("missing");
        assert_eq!(canonicalize(&missing).unwrap_err().kind(), std::fs::canonicalize(&missing).unwrap_err().kind());
    }
}
