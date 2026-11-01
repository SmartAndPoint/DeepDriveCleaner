//! What a file is beyond std's portable `Metadata`, on Windows: identity,
//! links and allocation from the file's own handle, and the few `Metadata`
//! accessors the seam pins.
//!
//! - **Identity** is `FileIdInfo`: the 64-bit volume serial and the 128-bit
//!   file id. A file id whose upper 64 bits are not zero (ReFS can make one)
//!   does not fit `(dev, ino)`; it is refused, never truncated (seam ADR §4,
//!   amended for Windows), so such a file cannot be planned.
//! - **Links and allocation** are `FileStandardInfo`: `NumberOfLinks` and
//!   `AllocationSize`, the bytes NTFS reserves on disk.
//! - Opened with `FILE_FLAG_BACKUP_SEMANTICS`, so folders open too; the
//!   backup privilege is never enabled, so no ACL is bypassed. `lstat` adds
//!   `FILE_FLAG_OPEN_REPARSE_POINT` and stats a link itself.

use std::fs::{Metadata, Permissions};
use std::io;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::Path;
use std::time::UNIX_EPOCH;

use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_NO_RECALL, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
    FILE_INFO_BY_HANDLE_CLASS, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_STANDARD_INFO, FileIdInfo, FileStandardInfo, GetFileInformationByHandleEx, OPEN_EXISTING,
};

use super::wide;
use crate::platform::types::FileStat;

/// The path's own facts, links not followed.
pub fn lstat(path: &Path) -> io::Result<FileStat> {
    stat_of(open(path, FILE_FLAG_OPEN_REPARSE_POINT)?)
}

/// The facts of what the path names, links followed.
pub fn stat(path: &Path) -> io::Result<FileStat> {
    stat_of(open(path, 0)?)
}

/// A listed entry's facts, links not followed: by its path, which this
/// module's calls reach past `MAX_PATH` in the verbatim form.
pub fn entry(entry: &std::fs::DirEntry) -> io::Result<FileStat> {
    lstat(&entry.path())
}

/// A handle that may read attributes and nothing else, sharing everything,
/// so it never stands in the way of another program's write or delete, and
/// never downloading a cloud placeholder (`FILE_FLAG_OPEN_NO_RECALL`).
fn open(path: &Path, extra: u32) -> io::Result<OwnedHandle> {
    let name = wide::verbatim(path)?;
    // SAFETY: `name` is NUL-terminated UTF-16 with no interior NUL
    // (`wide::verbatim` refuses one) that outlives the call; the security
    // attributes and template are null, which CreateFileW allows.
    let raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_NO_RECALL | extra,
            std::ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a valid handle CreateFileW just returned, owned by nobody else.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

/// Everything from the one handle, so the identity and the kind describe
/// the same object even if the path is swapped meanwhile. std's
/// `File::metadata` reads the handle (`GetFileInformationByHandle` and the
/// reparse tag), which `FILE_READ_ATTRIBUTES` allows.
fn stat_of(handle: OwnedHandle) -> io::Result<FileStat> {
    let (dev, ino) = id_of(&info::<FILE_ID_INFO>(&handle)?)?;
    let standard = info::<FILE_STANDARD_INFO>(&handle)?;
    let md = std::fs::File::from(handle).metadata()?;
    Ok(FileStat {
        dev,
        ino,
        nlink: u64::from(standard.NumberOfLinks),
        alloc: u64::try_from(standard.AllocationSize).unwrap_or(0),
        md,
    })
}

/// `(dev, ino)` from `FileIdInfo`, refused when it cannot name one file:
/// a 128-bit id (upper 64 bits set, ReFS) does not fit and is never
/// truncated; 0 and all ones (`FILE_INVALID_FILE_ID`, and what some
/// redirectors give every file) would make every file the same one.
fn id_of(info: &FILE_ID_INFO) -> io::Result<(u64, u64)> {
    let (low, high) = info.FileId.Identifier.split_at(8);
    if high.iter().any(|b| *b != 0) {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "a 128-bit file id does not fit (dev, ino)"));
    }
    let ino = u64::from_le_bytes(low.try_into().expect("eight bytes"));
    if ino == 0 || ino == u64::MAX {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "the file system gives no usable file id"));
    }
    Ok((info.VolumeSerialNumber, ino))
}

/// A `GetFileInformationByHandleEx` class and the struct it fills, tied
/// together so a class can never be read into the wrong struct.
trait Class: Copy {
    const CLASS: FILE_INFO_BY_HANDLE_CLASS;
}

impl Class for FILE_ID_INFO {
    const CLASS: FILE_INFO_BY_HANDLE_CLASS = FileIdInfo;
}

impl Class for FILE_STANDARD_INFO {
    const CLASS: FILE_INFO_BY_HANDLE_CLASS = FileStandardInfo;
}

fn info<T: Class>(handle: &OwnedHandle) -> io::Result<T> {
    let mut out = std::mem::MaybeUninit::<T>::zeroed();
    let size = u32::try_from(std::mem::size_of::<T>()).expect("a small struct");
    // SAFETY: `out` is a writable buffer of exactly `size` bytes for `T`,
    // the struct of class `T::CLASS`, and the handle is open for the call.
    let ok = unsafe { GetFileInformationByHandleEx(handle.as_raw_handle(), T::CLASS, out.as_mut_ptr().cast(), size) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the call succeeded and filled the whole struct. Its `BOOLEAN`
    // fields (`FILE_STANDARD_INFO`'s `DeletePending`, `Directory`, `bool`
    // in windows-sys) are written as 0 or 1 by the kernel, as std assumes.
    Ok(unsafe { out.assume_init() })
}

/// Modification time, Unix seconds.
pub fn mtime(md: &Metadata) -> i64 {
    match md.modified().map(|t| t.duration_since(UNIX_EPOCH)) {
        Ok(Ok(after)) => i64::try_from(after.as_secs()).unwrap_or(i64::MAX),
        // Floored, as Unix's `st_mtime` is: 1.5 s before is -2.
        Ok(Err(before)) => {
            let d = before.duration();
            -i64::try_from(d.as_secs()).unwrap_or(i64::MAX) - i64::from(d.subsec_nanos() > 0)
        }
        Err(_) => 0,
    }
}

/// No `st_flags` on Windows; cloud placeholders are told by their
/// attributes in the walker, not here.
pub fn st_flags(_md: &Metadata) -> u32 {
    0
}

/// Not reported: a Windows owner is a SID, not a user id (the owner check
/// of elevated sessions is its own step of the story).
pub fn owner(_md: &Metadata) -> Option<u32> {
    None
}

/// Not reported, as [`owner`]: a folder that must be the user's is not.
pub fn uid(_md: &Metadata) -> Option<u32> {
    None
}

/// Clears the read-only attribute, the one permission bit Windows has.
pub fn add_owner_rwx(perms: &mut Permissions) {
    perms.set_readonly(false);
}

/// No tool is found yet: which files the command runner may run (by
/// extension) is step 9 of story "Windows scanner and rule catalog".
pub fn is_executable(_md: &Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::platform::{skipped_without, testing};

    #[test]
    fn a_file_has_one_link_and_its_identity_from_any_spelling() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("a.bin");
        fs::write(&f, vec![1u8; 10_000]).unwrap();
        let st = lstat(&f).unwrap();
        assert_eq!(st.nlink, 1);
        assert!(st.alloc >= 10_000, "{}", st.alloc);
        assert_eq!(st.md.len(), 10_000);
        let upper = d.path().join("A.BIN");
        assert_eq!(lstat(&upper).unwrap().id(), st.id(), "another case names the same file");
        assert_eq!(stat(&f).unwrap().id(), st.id());
    }

    #[test]
    fn a_hard_link_shares_the_identity_and_counts_two() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        fs::write(&a, b"x").unwrap();
        let b = d.path().join("b");
        fs::hard_link(&a, &b).unwrap();
        let (sa, sb) = (lstat(&a).unwrap(), lstat(&b).unwrap());
        assert_eq!(sa.id(), sb.id());
        assert_eq!((sa.nlink, sb.nlink), (2, 2));
    }

    #[test]
    fn a_folder_opens_and_has_its_own_identity() {
        let d = tempfile::tempdir().unwrap();
        let sub = d.path().join("sub");
        fs::create_dir(&sub).unwrap();
        let (top, inner) = (lstat(d.path()).unwrap(), lstat(&sub).unwrap());
        assert!(inner.md.is_dir());
        assert_eq!(top.dev, inner.dev, "one volume");
        assert_ne!(top.ino, inner.ino);
    }

    /// `lstat` stats the link, `stat` what it names.
    #[test]
    fn lstat_stats_a_link_and_stat_its_target() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("target");
        fs::create_dir(&target).unwrap();
        let link = d.path().join("link");
        if skipped_without("lstat_stats_a_link_and_stat_its_target", testing::symlink(&target, &link)).is_none() {
            return;
        }
        assert_ne!(lstat(&link).unwrap().id(), lstat(&target).unwrap().id());
        assert_eq!(stat(&link).unwrap().id(), lstat(&target).unwrap().id());
    }

    #[test]
    fn a_path_longer_than_max_path_is_read() {
        let d = tempfile::tempdir().unwrap();
        let mut deep = d.path().to_path_buf();
        while deep.as_os_str().len() < 300 {
            deep.push("a-forty-character-folder-name-to-go-deep");
        }
        fs::create_dir_all(&deep).unwrap();
        let f = deep.join("f");
        fs::write(&f, b"x").unwrap();
        assert_eq!(lstat(&f).unwrap().nlink, 1);
    }

    fn id_info(serial: u64, bytes: [u8; 16]) -> FILE_ID_INFO {
        use windows_sys::Win32::Storage::FileSystem::FILE_ID_128;
        FILE_ID_INFO { VolumeSerialNumber: serial, FileId: FILE_ID_128 { Identifier: bytes } }
    }

    #[test]
    fn a_file_id_fits_only_in_64_bits_and_names_one_file() {
        let mut low = [0u8; 16];
        low[0] = 0x2a;
        low[7] = 0x01;
        assert_eq!(id_of(&id_info(7, low)).unwrap(), (7, 0x0100_0000_0000_002a));
        let mut wide = low;
        wide[8] = 1;
        assert_eq!(id_of(&id_info(7, wide)).unwrap_err().kind(), io::ErrorKind::Unsupported, "128-bit");
        assert!(id_of(&id_info(7, [0; 16])).is_err(), "zero");
        let mut ones = [0u8; 16];
        ones[..8].fill(0xff);
        assert!(id_of(&id_info(7, ones)).is_err(), "all ones");
    }

    /// A junction, which needs no privilege, is a link: `lstat` gives its
    /// own identity and a link's kind, `stat` the target's.
    #[test]
    fn a_junction_is_a_link_to_lstat_and_its_target_to_stat() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("target");
        fs::create_dir(&target).unwrap();
        let junction = d.path().join("junction");
        let made = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(&junction)
            .arg(&target)
            .output()
            .unwrap();
        assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
        let own = lstat(&junction).unwrap();
        assert!(own.md.file_type().is_symlink(), "a junction is a name surrogate");
        assert_ne!(own.id(), lstat(&target).unwrap().id());
        assert_eq!(stat(&junction).unwrap().id(), lstat(&target).unwrap().id());
    }

    /// `foo.` is its own file, never its sibling `foo`, as Win32 parsing
    /// would make it.
    #[test]
    fn a_trailing_dot_names_its_own_file() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("foo"), b"plain").unwrap();
        let mut dotted = std::ffi::OsString::from(r"\\?\");
        dotted.push(d.path().join("foo."));
        fs::write(&dotted, b"dotted name").unwrap();
        let (plain, dot) = (lstat(&d.path().join("foo")).unwrap(), lstat(&d.path().join("foo.")).unwrap());
        assert_ne!(plain.id(), dot.id());
        assert_eq!(dot.md.len(), 11);
    }

    #[test]
    fn mtime_is_unix_seconds() {
        let d = tempfile::tempdir().unwrap();
        let now = std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let t = mtime(&fs::metadata(d.path()).unwrap());
        assert!((now - t).abs() < 120, "{t} vs {now}");
    }
}
