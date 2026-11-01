//! Directory operations relative to a verified parent handle, on Windows
//! (safety ADR §5; story "Windows scanner and rule catalog", step 5): the
//! executor acts on exactly the folder it checked, never on a path resolved
//! again.
//!
//! - [`DirFd::open`] opens the folder itself, never through a link: a name
//!   surrogate (symlink, junction, mount point) is refused, as `O_NOFOLLOW`
//!   refuses a symlink on Unix.
//! - A child is opened by its name relative to the parent's handle
//!   (`NtCreateFile` with `RootDirectory`), with `FILE_OPEN_REPARSE_POINT`,
//!   so a child that is a link is the link, never its target.
//! - [`DirFd::rename_child`] renames inside the parent, through the NT call
//!   (`FileRenameInformation` with a simple name rooted at the parent's
//!   handle), never replacing an existing name.
//! - [`DirFd::unlink_child`] deletes a non-folder (`FileDispositionInfoEx`,
//!   POSIX semantics; `FileDispositionInfo` where refused).
//! - Every handle shares read, write and delete, and none recalls a cloud
//!   placeholder.

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::fs::{FileTimesExt as _, OpenOptionsExt as _};
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::Path;
use std::time::SystemTime;

use windows_sys::Wdk::Foundation::OBJECT_ATTRIBUTES;
use windows_sys::Wdk::Storage::FileSystem::{
    FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_NO_RECALL, FILE_OPEN_REPARSE_POINT, FILE_RENAME_INFORMATION,
    FILE_SYNCHRONOUS_IO_NONALERT, FileRenameInformation, NtCreateFile, NtSetInformationFile,
};
use windows_sys::Win32::Foundation::{
    ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, HANDLE, INVALID_HANDLE_VALUE,
    OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, DELETE, FILE_ADD_FILE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ATTRIBUTE_TAG_INFO, FILE_BASIC_INFO, FILE_DISPOSITION_FLAG_DELETE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_NO_RECALL,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA,
    FileDispositionInfo, FileDispositionInfoEx, OPEN_EXISTING, SYNCHRONIZE, SetFileInformationByHandle,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use super::meta::{id_of, info};
use crate::platform::types::EntryStat;

/// `ERROR_DIRECTORY`: the name is not a folder.
const NOT_A_DIRECTORY: i32 = 267;
/// `ERROR_CANT_RESOLVE_FILENAME`: what opening a link as a folder gives,
/// the counterpart of `ELOOP`.
const A_LINK: i32 = 1921;

/// Sets the modification time and the creation time (Windows' date added)
/// of `path`, without following a link. For tests: an entry written by a
/// test is "recent".
pub fn set_times(path: &Path, modified: SystemTime, added: SystemTime) -> io::Result<()> {
    let file = std::fs::OpenOptions::new()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    file.set_times(std::fs::FileTimes::new().set_modified(modified).set_created(added))
}

/// Opens `path` to read, without following a link and without recalling a
/// cloud placeholder. Windows has no FIFOs on its file systems, so nothing
/// can block the open.
pub fn open_nofollow_nonblock(path: &Path) -> io::Result<File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OPEN_NO_RECALL)
        .open(path)?;
    if file.metadata()?.file_type().is_symlink() {
        return Err(io::Error::from_raw_os_error(A_LINK));
    }
    Ok(file)
}

/// Whether the user may write in `path`, as `access(W_OK)` answers on
/// Unix: an open asking to add a file (a folder) or to write data (a file)
/// succeeds, which checks the ACL and a read-only volume. The read-only
/// attribute is not consulted for folders: Windows ignores it there, and
/// Explorer sets it on customised folders such as Documents. `false` when
/// the path does not exist.
pub fn can_write(path: &Path) -> bool {
    let Ok(md) = std::fs::metadata(path) else { return false };
    let access = if md.is_dir() { FILE_ADD_FILE } else { FILE_WRITE_DATA };
    std::fs::OpenOptions::new()
        .access_mode(access)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_NO_RECALL)
        .open(path)
        .is_ok()
}

/// An open folder, opened without following a link.
#[derive(Debug)]
pub struct DirFd(OwnedHandle);

impl DirFd {
    pub fn open(path: &Path) -> io::Result<Self> {
        let name = super::wide::verbatim(path)?;
        // SAFETY: `name` is NUL-terminated UTF-16 without an interior NUL
        // that outlives the call; null security attributes and template.
        let raw = unsafe {
            CreateFileW(
                name.as_ptr(),
                FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | SYNCHRONIZE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_OPEN_NO_RECALL,
                std::ptr::null_mut(),
            )
        };
        if raw == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a valid handle CreateFileW just returned, owned by nobody else.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
        let tag = info::<FILE_ATTRIBUTE_TAG_INFO>(&handle)?;
        if is_link(&tag) {
            return Err(io::Error::from_raw_os_error(A_LINK));
        }
        if tag.FileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(io::Error::from_raw_os_error(NOT_A_DIRECTORY));
        }
        Ok(Self(handle))
    }

    /// `(dev, ino)` of the folder itself.
    pub fn identity(&self) -> io::Result<(u64, u64)> {
        id_of(&info::<FILE_ID_INFO>(&self.0)?)
    }

    pub fn stat_child(&self, name: &OsStr) -> io::Result<EntryStat> {
        let child = self.child(name, FILE_READ_ATTRIBUTES | SYNCHRONIZE, 0)?;
        let (dev, ino) = id_of(&info::<FILE_ID_INFO>(&child)?)?;
        let tag = info::<FILE_ATTRIBUTE_TAG_INFO>(&child)?;
        let standard = info::<FILE_STANDARD_INFO>(&child)?;
        let is_symlink = is_link(&tag);
        let is_dir = !is_symlink && tag.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        Ok(EntryStat {
            dev,
            ino,
            is_dir,
            is_file: !is_symlink && !is_dir,
            is_symlink,
            flags: 0,
            alloc: u64::try_from(standard.AllocationSize).unwrap_or(0),
        })
    }

    /// The child's modification and creation times, Unix seconds, without
    /// following a link. Creation is what Windows keeps closest to a date
    /// added.
    pub fn child_times(&self, name: &OsStr) -> io::Result<(Option<i64>, Option<i64>)> {
        let child = self.child(name, FILE_READ_ATTRIBUTES | SYNCHRONIZE, 0)?;
        let basic = info::<FILE_BASIC_INFO>(&child)?;
        Ok((Some(unix_seconds(basic.LastWriteTime)), Some(unix_seconds(basic.CreationTime))))
    }

    /// Renames `from` to `to` inside this folder; fails if `to` exists.
    /// The child is opened by name relative to this folder's handle, then
    /// renamed in place through that handle, so neither path is resolved
    /// again.
    pub fn rename_child(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        single_name(to)?;
        let child = self.child(from, DELETE | SYNCHRONIZE, 0)?;
        set_rename(&child, &self.0, to)
    }

    /// Deletes the child `name` of this folder, which must not be a folder
    /// (`FILE_NON_DIRECTORY_FILE`). The only removal outside the executor:
    /// `ddc keep remove` calls it once, for `.ddc-keep`, after
    /// [`Self::stat_child`] said it is a regular file (safety ADR, amendment
    /// of 2026-10-30).
    pub fn unlink_child(&self, name: &OsStr) -> io::Result<()> {
        let child = self.child(name, DELETE | SYNCHRONIZE, FILE_NON_DIRECTORY_FILE)?;
        let ex =
            FILE_DISPOSITION_INFO_EX { Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS };
        match set_info(&child, FileDispositionInfoEx, &ex) {
            Err(e) if refused_class(&e) => {
                set_info(&child, FileDispositionInfo, &FILE_DISPOSITION_INFO { DeleteFile: true })
            }
            done => done,
        }
    }

    /// Opens the child `name` by name relative to this folder's handle, the
    /// link itself if it is one.
    fn child(&self, name: &OsStr, access: u32, options: u32) -> io::Result<OwnedHandle> {
        single_name(name)?;
        let mut wide: Vec<u16> = name.encode_wide().collect();
        let bytes = u16::try_from(wide.len() * 2).map_err(|_| io::Error::from_raw_os_error(206))?;
        let object = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: wide.as_mut_ptr() };
        let attributes = OBJECT_ATTRIBUTES {
            Length: u32::try_from(std::mem::size_of::<OBJECT_ATTRIBUTES>()).expect("a small struct"),
            RootDirectory: self.0.as_raw_handle(),
            ObjectName: &object,
            Attributes: OBJ_CASE_INSENSITIVE,
            SecurityDescriptor: std::ptr::null(),
            SecurityQualityOfService: std::ptr::null(),
        };
        let mut handle: HANDLE = std::ptr::null_mut();
        // SAFETY: IO_STATUS_BLOCK is plain data (a union of a status and a
        // pointer, and a length); all zeroes is a valid value.
        let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
        // SAFETY: every pointer is to a local that outlives the call: the
        // name's buffer (`wide`), its UNICODE_STRING, the OBJECT_ATTRIBUTES
        // naming the parent handle (open for the call), the out handle and
        // the status block. No allocation size, no extended attributes.
        let status = unsafe {
            NtCreateFile(
                &mut handle,
                access,
                &attributes,
                &mut status_block,
                std::ptr::null(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_OPEN,
                FILE_OPEN_REPARSE_POINT | FILE_OPEN_NO_RECALL | FILE_SYNCHRONOUS_IO_NONALERT | options,
                std::ptr::null(),
                0,
            )
        };
        nt(status)?;
        // SAFETY: NtCreateFile succeeded and returned a handle nobody else owns.
        Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
    }
}

/// An NT status as an `io::Error` with its Win32 code, so std's kinds
/// apply (`STATUS_OBJECT_NAME_COLLISION` is `AlreadyExists`).
fn nt(status: i32) -> io::Result<()> {
    if status >= 0 {
        return Ok(());
    }
    // SAFETY: a pure conversion of a status code.
    let code = unsafe { RtlNtStatusToDosError(status) };
    Err(io::Error::from_raw_os_error(i32::try_from(code).unwrap_or(i32::MAX)))
}

/// A name surrogate: a symlink, a junction or a mount point. Cloud
/// placeholders, WOF-compressed files and other tags are not links.
fn is_link(tag: &FILE_ATTRIBUTE_TAG_INFO) -> bool {
    // `IsReparseTagNameSurrogate`: bit 29 of the tag.
    tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 && tag.ReparseTag & 0x2000_0000 != 0
}

/// One name, never a path: no separator, not `.` or `..`, not empty, and
/// no `:`, which NT reads as a stream: relative to a folder, `:x` is a stream
/// of that folder and `:$I30:$INDEX_ALLOCATION` the folder itself.
fn single_name(name: &OsStr) -> io::Result<()> {
    let units: Vec<u16> = name.encode_wide().collect();
    let bad = units.is_empty()
        || units.iter().any(|u| [u16::from(b'\\'), u16::from(b'/'), u16::from(b':'), 0].contains(u))
        || units == [u16::from(b'.')]
        || units == [u16::from(b'.'), u16::from(b'.')];
    if bad {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a single name"));
    }
    Ok(())
}

/// The volume refused the extended information class (an older Windows, a
/// file system without POSIX semantics, such as FAT).
fn refused_class(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(c) if c == ERROR_INVALID_PARAMETER as i32 || c == ERROR_NOT_SUPPORTED as i32 || c == ERROR_INVALID_FUNCTION as i32)
}

/// Renames the open file to `to` inside `parent`, through the NT call: a
/// simple name with the parent's handle as `RootDirectory`, so the
/// destination is the verified folder whatever happened to the item since
/// it was opened. The Win32 `SetFileInformationByHandle` is not used: it
/// resolves a bare name against the process's current folder and would move
/// the item there. `ReplaceIfExists` is false: an existing name is never
/// replaced (`STATUS_OBJECT_NAME_COLLISION`, `AlreadyExists`).
fn set_rename(handle: &OwnedHandle, parent: &OwnedHandle, to: &OsStr) -> io::Result<()> {
    let name: Vec<u16> = to.encode_wide().collect();
    let header = std::mem::offset_of!(FILE_RENAME_INFORMATION, FileName);
    // The whole struct, then the name and a closing NUL: at least the
    // documented `size_of + FileNameLength`.
    let size = std::mem::size_of::<FILE_RENAME_INFORMATION>() + (name.len() + 1) * 2;
    let mut buffer = vec![0u64; size.div_ceil(8)];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    // SAFETY: `buffer` holds `size` bytes, more than the header, the name
    // and its NUL need, and is aligned to 8, more than the struct needs. The
    // writes stay inside it: `ReplaceIfExists` false, the parent's handle,
    // the name length in bytes (without the NUL), then the name's units from
    // the `FileName` offset on; the zeroed buffer supplies the NUL.
    unsafe {
        (*info).Anonymous.ReplaceIfExists = false;
        (*info).RootDirectory = parent.as_raw_handle();
        (*info).FileNameLength = u32::try_from(name.len() * 2).expect("a short name");
        let dest = buffer.as_mut_ptr().cast::<u8>().add(header).cast::<u16>();
        std::ptr::copy_nonoverlapping(name.as_ptr(), dest, name.len());
    }
    let len = u32::try_from(size).expect("a short name");
    // SAFETY: IO_STATUS_BLOCK is plain data; all zeroes is a valid value.
    let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
    // SAFETY: the buffer is a filled FILE_RENAME_INFORMATION of `len` bytes,
    // the status block is a local, and both handles (the item's, opened with
    // DELETE, and the parent's) are open for the call.
    let status = unsafe {
        NtSetInformationFile(
            handle.as_raw_handle(),
            &mut status_block,
            buffer.as_ptr().cast(),
            len,
            FileRenameInformation,
        )
    };
    nt(status)
}

fn set_info<T>(handle: &OwnedHandle, class: FILE_INFO_BY_HANDLE_CLASS, value: &T) -> io::Result<()> {
    let len = u32::try_from(std::mem::size_of::<T>()).expect("a small struct");
    // SAFETY: `value` is the struct of `class`, `len` bytes, and the handle
    // is open for the call.
    let ok = unsafe { SetFileInformationByHandle(handle.as_raw_handle(), class, (value as *const T).cast(), len) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// A FILETIME (100 ns since 1601) as Unix seconds, floored.
fn unix_seconds(filetime: i64) -> i64 {
    const SECONDS_1601_TO_1970: i64 = 11_644_473_600;
    filetime.div_euclid(10_000_000) - SECONDS_1601_TO_1970
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::platform::meta;

    fn junction(link: &Path, target: &Path) {
        let made =
            std::process::Command::new("cmd").arg("/C").arg("mklink").arg("/J").arg(link).arg(target).output().unwrap();
        assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
    }

    #[test]
    fn the_folder_and_its_children_are_stated_as_meta_states_them() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("f"), vec![1u8; 5000]).unwrap();
        fs::create_dir(d.path().join("sub")).unwrap();
        junction(&d.path().join("j"), &d.path().join("sub"));
        let dir = DirFd::open(d.path()).unwrap();
        assert_eq!(dir.identity().unwrap(), meta::lstat(d.path()).unwrap().id());
        let f = dir.stat_child(OsStr::new("F")).unwrap();
        assert!(f.is_file && !f.is_dir && !f.is_symlink);
        assert_eq!((f.dev, f.ino), meta::lstat(&d.path().join("f")).unwrap().id());
        assert!(f.alloc >= 5000);
        let sub = dir.stat_child(OsStr::new("sub")).unwrap();
        assert!(sub.is_dir && !sub.is_symlink);
        let j = dir.stat_child(OsStr::new("j")).unwrap();
        assert!(j.is_symlink && !j.is_dir && !j.is_file, "a junction is a link, never its target");
        assert_ne!((j.dev, j.ino), (sub.dev, sub.ino));
    }

    #[test]
    fn a_link_or_a_file_is_not_opened_as_a_folder() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("sub")).unwrap();
        junction(&d.path().join("j"), &d.path().join("sub"));
        fs::write(d.path().join("f"), b"x").unwrap();
        assert_eq!(DirFd::open(&d.path().join("j")).unwrap_err().raw_os_error(), Some(A_LINK));
        assert_eq!(DirFd::open(&d.path().join("f")).unwrap_err().raw_os_error(), Some(NOT_A_DIRECTORY));
    }

    #[test]
    fn a_rename_stays_in_its_folder_and_never_replaces() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("a")).unwrap();
        fs::write(d.path().join("a/inside"), b"x").unwrap();
        fs::write(d.path().join("taken"), b"y").unwrap();
        let dir = DirFd::open(d.path()).unwrap();
        assert_eq!(
            dir.rename_child(OsStr::new("a"), OsStr::new("taken")).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        dir.rename_child(OsStr::new("a"), OsStr::new("a.tomb")).unwrap();
        assert!(d.path().join("a.tomb/inside").exists() && !d.path().join("a").exists());
        assert!(dir.rename_child(OsStr::new("a.tomb"), OsStr::new(r"..\out")).is_err(), "a path is refused");
    }

    #[test]
    fn renaming_a_junction_renames_the_link_not_its_target() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("target")).unwrap();
        fs::write(d.path().join("target/keep"), b"x").unwrap();
        junction(&d.path().join("j"), &d.path().join("target"));
        let dir = DirFd::open(d.path()).unwrap();
        dir.rename_child(OsStr::new("j"), OsStr::new("j.tomb")).unwrap();
        assert!(d.path().join("target/keep").exists(), "the target stays where it was");
        assert!(meta::lstat(&d.path().join("j.tomb")).unwrap().md.file_type().is_symlink());
    }

    #[test]
    fn unlink_removes_a_file_and_refuses_a_folder() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join(".ddc-keep"), b"").unwrap();
        fs::create_dir(d.path().join("sub")).unwrap();
        let dir = DirFd::open(d.path()).unwrap();
        dir.unlink_child(OsStr::new(".ddc-keep")).unwrap();
        assert!(!d.path().join(".ddc-keep").exists());
        assert!(dir.unlink_child(OsStr::new("sub")).is_err());
        assert!(d.path().join("sub").is_dir());
    }

    #[test]
    fn times_are_set_and_read_back_in_unix_seconds() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        fs::write(&f, b"x").unwrap();
        let modified = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let added = UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        set_times(&f, modified, added).unwrap();
        let dir = DirFd::open(d.path()).unwrap();
        assert_eq!(dir.child_times(OsStr::new("f")).unwrap(), (Some(1_700_000_000), Some(1_600_000_000)));
        assert_eq!(unix_seconds(0), -11_644_473_600);
        assert_eq!(unix_seconds(-1), -11_644_473_601, "floored");
    }

    #[test]
    fn a_read_only_file_is_not_writable_and_a_link_is_not_opened() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        fs::write(&f, b"x").unwrap();
        assert!(can_write(&f));
        let mut perms = fs::metadata(&f).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&f, perms).unwrap();
        assert!(!can_write(&f));
        assert!(!can_write(&d.path().join("none")));
        assert!(open_nofollow_nonblock(&f).is_ok());
        fs::create_dir(d.path().join("sub")).unwrap();
        junction(&d.path().join("j"), &d.path().join("sub"));
        // Refused either way: as a link, or (a folder junction opened as a
        // file) as access denied; never followed.
        assert!(open_nofollow_nonblock(&d.path().join("j")).is_err());
    }

    #[test]
    fn a_stream_a_path_or_a_missing_name_is_not_a_child() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("a")).unwrap();
        let dir = DirFd::open(d.path()).unwrap();
        for name in [":x", ":$I30:$INDEX_ALLOCATION", "a:b", r"a\b", "..", "."] {
            assert!(dir.stat_child(OsStr::new(name)).is_err(), "{name}");
        }
        assert!(dir.rename_child(OsStr::new("a"), OsStr::new("a:b")).is_err(), "a stream as the new name");
        assert!(dir.rename_child(OsStr::new(r"..\a"), OsStr::new("b")).is_err(), "a path as the old name");
        assert_eq!(dir.stat_child(OsStr::new("gone")).unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(d.path().join("a").is_dir());
    }

    /// A file symlink, where the privilege allows one, is refused as a link.
    #[test]
    fn a_file_symlink_is_not_opened_to_read() {
        let d = tempfile::tempdir().unwrap();
        let target = d.path().join("target");
        fs::write(&target, b"x").unwrap();
        let link = d.path().join("link");
        let made = crate::platform::testing::symlink(&target, &link);
        if crate::platform::skipped_without("a_file_symlink_is_not_opened_to_read", made).is_none() {
            return;
        }
        assert_eq!(open_nofollow_nonblock(&link).unwrap_err().raw_os_error(), Some(A_LINK));
    }

    #[test]
    fn a_folder_takes_times_and_a_read_only_attribute_leaves_it_writable() {
        let d = tempfile::tempdir().unwrap();
        let sub = d.path().join("sub");
        fs::create_dir(&sub).unwrap();
        let modified = UNIX_EPOCH + Duration::from_secs(1_650_000_000);
        set_times(&sub, modified, modified).unwrap();
        assert_eq!(DirFd::open(d.path()).unwrap().child_times(OsStr::new("sub")).unwrap().0, Some(1_650_000_000));
        let mut perms = fs::metadata(&sub).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(&sub, perms).unwrap();
        assert!(can_write(&sub), "Windows ignores the read-only attribute on a folder");
    }

    /// A WOF-compressed file reads as its content through the no-follow
    /// open, not as the raw stream behind its reparse point.
    #[test]
    fn a_compressed_file_reads_as_its_content() {
        use std::io::Read as _;
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("big.txt");
        let content = "compressible line of text\n".repeat(20_000);
        fs::write(&f, &content).unwrap();
        let compacted = std::process::Command::new("compact").arg("/c").arg("/exe:xpress4k").arg(&f).output();
        let Ok(out) = compacted else { return };
        if !out.status.success() {
            return;
        }
        let mut read = String::new();
        open_nofollow_nonblock(&f).unwrap().read_to_string(&mut read).unwrap();
        assert_eq!(read.len(), content.len());
        assert!(read == content);
    }
}
