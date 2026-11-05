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
    FILE_INFORMATION_CLASS, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_NO_RECALL, FILE_OPEN_REPARSE_POINT,
    FILE_RENAME_INFORMATION, FILE_STAT_INFORMATION, FILE_SYNCHRONOUS_IO_NONALERT, FileRenameInformation,
    FileStatInformation, NtCreateFile, NtSetInformationFile,
};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, ERROR_SHARING_VIOLATION,
    HANDLE, INVALID_HANDLE_VALUE, OBJ_CASE_INSENSITIVE, RtlNtStatusToDosError, UNICODE_STRING,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, DELETE, FILE_ADD_FILE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_READONLY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_TAG_INFO, FILE_BASIC_INFO, FILE_DISPOSITION_FLAG_DELETE,
    FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO,
    FILE_DISPOSITION_INFO_EX, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_NO_RECALL, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FILE_WRITE_ATTRIBUTES, FILE_WRITE_DATA, FileBasicInfo,
    FileDispositionInfo, FileDispositionInfoEx, OPEN_EXISTING, ReOpenFile, SYNCHRONIZE, SetFileInformationByHandle,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};

use super::meta::{id_of, info};
use crate::platform::types::{EntryKind, EntryStat, HeldOpen, Times};

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

/// Opens `path` to read a file a walk found, as [`open_nofollow_nonblock`]:
/// a cloud placeholder is never recalled, so a dehydrated file fails to read
/// instead of downloading, and a link is refused. Unlike Unix, where a
/// symlinked marker is followed, a Windows symlink may name a share on
/// another machine (the open would connect to it and offer the user's
/// credentials) or a named pipe that never answers; symlinked markers are
/// rare on Windows, so none is read.
pub fn open_read(path: &Path) -> io::Result<File> {
    open_nofollow_nonblock(path)
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

    /// `dir`, at or beneath `anchor`'s root and on its volume, opened as
    /// [`Self::open`] opens it, never through a link.
    pub fn open_beneath(anchor: &super::bulk::Anchor, dir: &Path) -> io::Result<Self> {
        if !dir.starts_with(&anchor.path) {
            return Err(io::Error::other("outside the scan root"));
        }
        let folder = Self::open(dir)?;
        if folder.identity()?.0 != anchor.dev {
            return Err(io::Error::other("another volume"));
        }
        Ok(folder)
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
            flags: super::meta::flags_of_handle(tag.FileAttributes),
            alloc: u64::try_from(standard.AllocationSize).unwrap_or(0),
            mount_root: false,
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
    ///
    /// A holder is judged by the stage that failed (safety ADR §5,
    /// amendment of 2026-11-02): a sharing violation at the open (a process
    /// works inside the folder) and access denied at the rename after a
    /// successful open (a file below is open) come back as an error carrying
    /// [`HeldOpen`]. Only the rename's denial is retried, [`RENAME_ATTEMPTS`]
    /// times in all, [`RENAME_PAUSE`] apart, for an antivirus scan's brief
    /// hold. Access denied at the open is an ACL and stays a plain error.
    pub fn rename_child(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        single_name(to)?;
        let child = self.child(from, DELETE | SYNCHRONIZE, 0).map_err(|e| held_open(Stage::Open, e))?;
        let mut attempt = 1;
        loop {
            match set_rename(&child, &self.0, to) {
                Err(e) if attempt < RENAME_ATTEMPTS && denied(&e) => {
                    attempt += 1;
                    std::thread::sleep(RENAME_PAUSE);
                }
                done => return done.map_err(|e| held_open(Stage::Rename, e)),
            }
        }
    }

    /// Removes the folder `name` of this directory and everything in it, by
    /// `path`: std's removal opens each entry relative to its folder's
    /// handle and never follows a link (safety ADR §5, amendment of
    /// 2026-11-04: Windows keeps it). Where it fails, the read-only
    /// attribute is cleared from the folders below, and from the files on
    /// FAT and exFAT, which honour it, and it is tried again; the first
    /// error is the one reported. `id` is the guard's, checked by the
    /// caller's rename. Only `exec::delete` calls it.
    #[allow(clippy::disallowed_methods)] // the one place that deletes directory trees on Windows
    pub fn remove_tree(&self, _name: &OsStr, path: &Path, _id: (u64, u64)) -> io::Result<()> {
        std::fs::remove_dir_all(path).or_else(|first| {
            folders_writable(path);
            let _ = clear_read_only(path);
            std::fs::remove_dir_all(path).map_err(|_| first)
        })
    }

    /// Deletes the child `name` of this folder, which must not be a folder
    /// (`FILE_NON_DIRECTORY_FILE`). The only removal outside the executor:
    /// `ddc keep remove` calls it once, for `.ddc-keep`, after
    /// [`Self::stat_child`] said it is a regular file (safety ADR, amendment
    /// of 2026-10-30).
    ///
    /// A read-only file is deleted as any other (`IGNORE_READONLY_ATTRIBUTE`).
    /// Where the volume refuses that call (FAT, exFAT, an older Windows), the
    /// plain disposition honours the attribute: it is cleared through a
    /// second handle to the same file, and put back if the delete still
    /// fails (story "Windows scanner and rule catalog", step 8b).
    pub fn unlink_child(&self, name: &OsStr) -> io::Result<()> {
        let child = self.child(name, DELETE | SYNCHRONIZE, FILE_NON_DIRECTORY_FILE)?;
        let ex = FILE_DISPOSITION_INFO_EX {
            Flags: FILE_DISPOSITION_FLAG_DELETE
                | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS
                | FILE_DISPOSITION_FLAG_IGNORE_READONLY_ATTRIBUTE,
        };
        match set_info(&child, FileDispositionInfoEx, &ex) {
            Err(e) if refused_class(&e) => Self::unlink_plain(&child),
            done => done,
        }
    }

    /// The plain disposition of `child`; a read-only attribute that refuses
    /// it is cleared through a second handle reopened from `child`, so on
    /// the same file by construction, with no name resolved again.
    fn unlink_plain(child: &OwnedHandle) -> io::Result<()> {
        let delete = || set_info(child, FileDispositionInfo, &FILE_DISPOSITION_INFO { DeleteFile: true });
        let refused = match delete() {
            Err(e) if denied(&e) => e,
            done => return done,
        };
        let Ok(attributes) = reopen(child, ATTRIBUTES) else { return Err(refused) };
        let Some(was) = clear_read_only_attribute(&attributes)? else { return Err(refused) };
        delete().inspect_err(|_| {
            let _ = set_attributes(&attributes, was);
        })
    }

    /// Clears the read-only attribute of the child `name`, opened relative
    /// to this folder as a plain file, the link itself if it is one, and
    /// changed only when it is not a link.
    fn clear_child_read_only(&self, name: &OsStr) -> io::Result<()> {
        let child = self.child(name, ATTRIBUTES, FILE_NON_DIRECTORY_FILE)?;
        clear_read_only_attribute(&child).map(drop)
    }

    /// The folder's handle, for the walker's listing (`bulk`).
    pub(super) fn handle(&self) -> &OwnedHandle {
        &self.0
    }

    /// Opens the child `name` by name relative to this folder's handle, the
    /// link itself if it is one.
    pub(super) fn child(&self, name: &OsStr, access: u32, options: u32) -> io::Result<OwnedHandle> {
        single_name(name)?;
        let mut wide: Vec<u16> = name.encode_wide().collect();
        let bytes = u16::try_from(wide.len() * 2).map_err(|_| io::Error::from_raw_os_error(206))?;
        let object = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: wide.as_mut_ptr() };
        let attributes = self.relative(&object, OBJ_CASE_INSENSITIVE);
        let mut handle: HANDLE = std::ptr::null_mut();
        // SAFETY: IO_STATUS_BLOCK is plain data (a union of a status and a
        // pointer, and a length); all zeroes is a valid value.
        let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
        // SAFETY: every pointer is to a local that outlives the call: the
        // name's buffer (`wide`), its UNICODE_STRING, the OBJECT_ATTRIBUTES
        // (raw pointers to both and to the parent handle, open for the call;
        // only this scope keeps them alive), the out handle and the status
        // block. No allocation size, no extended attributes.
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

    /// The child's id, attributes, sizes and link count in one call, by name
    /// relative to this folder, without opening a handle to it
    /// (`NtQueryInformationByName` with `FileStatInformation`, Windows 10
    /// 1709 and later). The function is looked up at run time, so the binary
    /// still starts on an older Windows: there, and where the class is
    /// refused, the error is `ERROR_INVALID_FUNCTION` or a refused-class code, and the caller opens
    /// the file instead. The caller compares `FileId` with what it listed,
    /// since the name can change between the listing and this call.
    pub(super) fn stat_by_name(&self, name: &OsStr) -> io::Result<FILE_STAT_INFORMATION> {
        let query = query_by_name().ok_or_else(|| io::Error::from_raw_os_error(ERROR_INVALID_FUNCTION as i32))?;
        single_name(name)?;
        let mut wide: Vec<u16> = name.encode_wide().collect();
        let bytes = u16::try_from(wide.len() * 2).map_err(|_| io::Error::from_raw_os_error(206))?;
        let object = UNICODE_STRING { Length: bytes, MaximumLength: bytes, Buffer: wide.as_mut_ptr() };
        let attributes = self.relative(&object, 0);
        let mut out = FILE_STAT_INFORMATION::default();
        // SAFETY: IO_STATUS_BLOCK is plain data; all zeroes is a valid value.
        let mut status_block: IO_STATUS_BLOCK = unsafe { std::mem::zeroed() };
        // SAFETY: `query` is ntdll's NtQueryInformationByName, of the stated
        // signature. Every pointer is to a local that outlives the call: the
        // name's buffer (`wide`) and its UNICODE_STRING, which the
        // OBJECT_ATTRIBUTES point at (raw pointers: nothing but this scope
        // keeps them alive), the parent handle (open for the call), the
        // status block and the output struct, whose size is the stated length.
        let status = unsafe {
            query(
                &attributes,
                &mut status_block,
                (&raw mut out).cast(),
                u32::try_from(size_of::<FILE_STAT_INFORMATION>()).expect("a small struct"),
                FileStatInformation,
            )
        };
        nt(status)?;
        Ok(out)
    }

    /// Object attributes naming `object` relative to this folder's handle.
    /// They hold raw pointers to both: the caller keeps the name, its
    /// UNICODE_STRING and this folder alive across the call that uses them.
    /// A name taken from a listing is matched exactly (`flags` 0), so in a
    /// case-sensitive folder it never finds the name's neighbour.
    fn relative(&self, object: &UNICODE_STRING, flags: u32) -> OBJECT_ATTRIBUTES {
        OBJECT_ATTRIBUTES {
            Length: u32::try_from(std::mem::size_of::<OBJECT_ATTRIBUTES>()).expect("a small struct"),
            RootDirectory: self.0.as_raw_handle(),
            ObjectName: object,
            Attributes: flags,
            SecurityDescriptor: std::ptr::null(),
            SecurityQualityOfService: std::ptr::null(),
        }
    }
}

pub(super) type QueryByName = unsafe extern "system" fn(
    *const OBJECT_ATTRIBUTES,
    *mut IO_STATUS_BLOCK,
    *mut core::ffi::c_void,
    u32,
    FILE_INFORMATION_CLASS,
) -> i32;

/// ntdll's `NtQueryInformationByName`, looked up once; `None` before
/// Windows 10 1703, which lacks it. A load-time import would stop the binary
/// from starting there at all.
pub(super) fn query_by_name() -> Option<QueryByName> {
    static QUERY: std::sync::OnceLock<Option<QueryByName>> = std::sync::OnceLock::new();
    *QUERY.get_or_init(|| {
        let ntdll: Vec<u16> = "ntdll.dll".encode_utf16().chain([0]).collect();
        // SAFETY: a NUL-terminated wide name; ntdll is mapped in every process
        // and never unloaded, so the module handle needs no release.
        let module = unsafe { GetModuleHandleW(ntdll.as_ptr()) };
        if module.is_null() {
            return None;
        }
        // SAFETY: a NUL-terminated ASCII name and a module handle that stays
        // valid for the process's life.
        let found = unsafe { GetProcAddress(module, c"NtQueryInformationByName".as_ptr().cast()) }?;
        // SAFETY: the export has this signature (ntifs.h); both are
        // `extern "system"` function pointers of the same size.
        Some(unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, QueryByName>(found) })
    })
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
    name_surrogate(tag.FileAttributes, tag.ReparseTag)
}

/// `IsReparseTagNameSurrogate` on a reparse point: bit 29 of the tag.
pub(super) fn name_surrogate(attributes: u32, tag: u32) -> bool {
    attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 && tag & 0x2000_0000 != 0
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

/// Clears the read-only attribute of every plain file below `root`, the
/// retry of a tree delete on FAT and exFAT (safety ADR §5, amendment of
/// 2026-11-02; story "Windows scanner and rule catalog", step 8b): there
/// std's delete falls back to the plain disposition, which honours the
/// attribute. On any other file system it changes nothing: NTFS and ReFS
/// take std's `IGNORE_READONLY_ATTRIBUTE`.
pub fn clear_read_only(root: &Path) -> io::Result<()> {
    if !is_fat(&super::statfs::statfs(root)?.fs_type) {
        return Ok(());
    }
    clear_read_only_below(root)
}

/// Clears the read-only attribute of every folder below `root`, never
/// through a link (Go marks its module cache's folders so).
fn folders_writable(root: &Path) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(md) = std::fs::symlink_metadata(&dir) else { continue };
        if !md.is_dir() {
            continue;
        }
        let mut perms = md.permissions();
        if perms.readonly() {
            perms.set_readonly(false);
            let _ = std::fs::set_permissions(&dir, perms);
        }
        if let Ok(rd) = crate::stats::read_dir(&dir) {
            stack.extend(
                rd.filter_map(Result::ok).filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()),
            );
        }
    }
}

/// FAT and exFAT, as `GetVolumeInformationW` names them (lowercased).
fn is_fat(fs_type: &str) -> bool {
    matches!(fs_type, "fat" | "fat12" | "fat16" | "fat32" | "exfat")
}

/// Each folder is opened by its own handle, never through a link, and kept
/// on `root`'s volume ([`super::bulk::list_in`], [`DirFd::open_beneath`]);
/// each file is opened relative to its folder. A link or another volume is
/// never entered, and a folder that cannot be read is passed over: the
/// delete that follows reports what is left.
fn clear_read_only_below(root: &Path) -> io::Result<()> {
    let anchor = super::bulk::anchor(root)?;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(listing) = super::bulk::list_in(&anchor, &dir, None, false, Times::None) else { continue };
        let Ok(folder) = DirFd::open_beneath(&anchor, &dir) else { continue };
        if folder.identity().ok() != Some((listing.dev, listing.ino)) {
            continue;
        }
        for entry in listing.entries {
            match entry.kind {
                // A mount point is another volume, never entered.
                EntryKind::Dir if entry.boundary.is_none() => stack.push(dir.join(&entry.name)),
                EntryKind::Dir => {}
                EntryKind::File => {
                    let _ = folder.clear_child_read_only(&entry.name);
                }
                EntryKind::Symlink | EntryKind::Other => {}
            }
        }
    }
    Ok(())
}

/// A second handle to the file `handle` is open on, with `access`
/// (`ReOpenFile`): the same file whatever its name now is, the link itself
/// if it is one. `ReOpenFile` refuses `FILE_FLAG_OPEN_NO_RECALL` (87); an
/// open for attributes only downloads no placeholder anyway.
fn reopen(handle: &OwnedHandle, access: u32) -> io::Result<OwnedHandle> {
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    // SAFETY: `handle` is open for the call; plain constants otherwise.
    let raw = unsafe { ReOpenFile(handle.as_raw_handle(), access, share, FILE_FLAG_OPEN_REPARSE_POINT) };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: ReOpenFile succeeded and returned a handle nobody else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

/// Reading and changing a file's attributes, nothing else.
const ATTRIBUTES: u32 = FILE_READ_ATTRIBUTES | FILE_WRITE_ATTRIBUTES | SYNCHRONIZE;

/// The attributes `FileBasicInformation` may set
/// (`FILE_ATTRIBUTE_VALID_SET_FLAGS`): read-only, hidden, system, archive,
/// normal, temporary, offline and not-content-indexed.
const SETTABLE: u32 = 0x31A7;

/// Clears `FILE_ATTRIBUTE_READONLY` on an open file that is not a link:
/// the attributes it had when it changed them, `None` when there was
/// nothing to change.
fn clear_read_only_attribute(handle: &OwnedHandle) -> io::Result<Option<u32>> {
    let basic = info::<FILE_BASIC_INFO>(handle)?;
    let attributes = basic.FileAttributes;
    if attributes & FILE_ATTRIBUTE_READONLY == 0 || attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Ok(None);
    }
    set_attributes(handle, attributes & !FILE_ATTRIBUTE_READONLY)?;
    Ok(Some(attributes))
}

/// Sets a file's attributes, keeping its times (a zero time is left as it
/// is) and its attributes that cannot be set.
fn set_attributes(handle: &OwnedHandle, attributes: u32) -> io::Result<()> {
    let settable = attributes & SETTABLE;
    let basic = FILE_BASIC_INFO {
        CreationTime: 0,
        LastAccessTime: 0,
        LastWriteTime: 0,
        ChangeTime: 0,
        // Zero would mean "unchanged": a file with none left is normal.
        FileAttributes: if settable == 0 { FILE_ATTRIBUTE_NORMAL } else { settable },
    };
    set_info(handle, FileBasicInfo, &basic)
}

/// How many times the tombstone rename is tried while it is denied after a
/// successful open, and the pause between tries (safety ADR §5, amendment of
/// 2026-11-02): a bounded wait for an antivirus scan, never a loop.
const RENAME_ATTEMPTS: u32 = 3;
const RENAME_PAUSE: std::time::Duration = std::time::Duration::from_millis(50);

/// The stage of [`DirFd::rename_child`] that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Opening the child with `DELETE`.
    Open,
    /// Renaming it through the open handle.
    Rename,
}

/// The error a stage returns: a sharing violation at the open and a denial
/// at the rename carry [`HeldOpen`]; everything else, a denial at the open
/// (an ACL) included, is the plain error.
fn held_open(stage: Stage, e: io::Error) -> io::Error {
    let held = match stage {
        Stage::Open if e.raw_os_error() == Some(ERROR_SHARING_VIOLATION as i32) => HeldOpen::AtOpen,
        Stage::Rename if denied(&e) => HeldOpen::AtRename,
        _ => return e,
    };
    io::Error::other(held)
}

fn denied(e: &io::Error) -> bool {
    e.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32)
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
pub(super) fn unix_seconds(filetime: i64) -> i64 {
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

    /// A folder with `inside` open by this process, sharing `share`.
    fn holding(share: u32) -> (tempfile::TempDir, File) {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("f")).unwrap();
        fs::write(d.path().join("f/inside"), b"x").unwrap();
        let open = fs::OpenOptions::new().read(true).share_mode(share).open(d.path().join("f/inside")).unwrap();
        (d, open)
    }

    /// A process working inside `dir` until it is killed: one `cmd`,
    /// waiting on its input, with no child of its own to outlive it. It
    /// opens its working folder while it starts, after `spawn` returns, so
    /// this waits for its first line, written once it runs.
    fn working_in(dir: &Path) -> std::process::Child {
        use std::io::BufRead as _;
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "echo ready& set /p wait="])
            .current_dir(dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut line = String::new();
        io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "ready");
        child
    }

    fn held(e: &io::Error) -> Option<HeldOpen> {
        e.get_ref().and_then(|p| p.downcast_ref::<HeldOpen>()).copied()
    }

    /// The lab matrix's first column: a process working inside the folder
    /// fails the open with a sharing violation, reported held open at once.
    #[test]
    fn a_folder_a_process_works_in_is_held_open_at_the_open() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        fs::create_dir(&f).unwrap();
        let mut child = working_in(&f);
        let dir = DirFd::open(d.path()).unwrap();
        let renamed = dir.rename_child(OsStr::new("f"), OsStr::new("f.tomb"));
        let _ = child.kill();
        let _ = child.wait();
        let e = renamed.unwrap_err();
        assert_eq!(held(&e), Some(HeldOpen::AtOpen), "{e}");
        assert!(HeldOpen::is(&e));
        assert!(f.exists(), "the folder keeps its name");
    }

    /// A file open below the folder, with or without delete sharing: the
    /// open succeeds, every rename is denied, and after the bounded retry
    /// it is reported held open.
    #[test]
    fn a_folder_with_an_open_file_below_is_held_open_at_the_rename() {
        for share in [FILE_SHARE_READ | FILE_SHARE_WRITE, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE] {
            let (d, open) = holding(share);
            let dir = DirFd::open(d.path()).unwrap();
            let started = std::time::Instant::now();
            let e = dir.rename_child(OsStr::new("f"), OsStr::new("f.tomb")).unwrap_err();
            assert_eq!(held(&e), Some(HeldOpen::AtRename), "share {share:#x}: {e}");
            assert!(started.elapsed() >= RENAME_PAUSE * (RENAME_ATTEMPTS - 1), "every attempt was made");
            assert!(d.path().join("f/inside").exists(), "the folder keeps its name");
            drop(open);
        }
    }

    /// A hold that ends between attempts, as an antivirus scan's does: the
    /// rename goes through on a later attempt.
    #[test]
    fn a_hold_that_ends_between_attempts_lets_the_rename_through() {
        let (d, open) = holding(FILE_SHARE_READ | FILE_SHARE_WRITE);
        let dir = DirFd::open(d.path()).unwrap();
        // Released inside the first pause: the first attempt is denied, a
        // later one goes through.
        let release = std::thread::spawn(move || {
            std::thread::sleep(RENAME_PAUSE / 2);
            drop(open);
        });
        let started = std::time::Instant::now();
        let renamed = dir.rename_child(OsStr::new("f"), OsStr::new("f.tomb"));
        let took = started.elapsed();
        release.join().unwrap();
        renamed.unwrap();
        assert!(took >= RENAME_PAUSE, "renamed on a retry, not at once ({took:?})");
        assert!(d.path().join("f.tomb/inside").exists());
    }

    /// Only the two held-open stages carry the marker: a denial at the open
    /// is an ACL, and every other error stays as it was.
    #[test]
    fn only_the_two_held_open_stages_carry_the_marker() {
        let code = |c: u32| io::Error::from_raw_os_error(c as i32);
        assert_eq!(held(&held_open(Stage::Open, code(ERROR_SHARING_VIOLATION))), Some(HeldOpen::AtOpen));
        assert_eq!(held(&held_open(Stage::Rename, code(ERROR_ACCESS_DENIED))), Some(HeldOpen::AtRename));
        let acl = held_open(Stage::Open, code(ERROR_ACCESS_DENIED));
        assert_eq!((held(&acl), acl.raw_os_error()), (None, Some(ERROR_ACCESS_DENIED as i32)));
        let busy = held_open(Stage::Rename, code(ERROR_SHARING_VIOLATION));
        assert_eq!((held(&busy), busy.raw_os_error()), (None, Some(ERROR_SHARING_VIOLATION as i32)));
        let taken = held_open(Stage::Rename, io::Error::from(io::ErrorKind::AlreadyExists));
        assert_eq!((held(&taken), taken.kind()), (None, io::ErrorKind::AlreadyExists));
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

    fn read_only(path: &Path) {
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(path, perms).unwrap();
    }

    fn is_read_only(path: &Path) -> bool {
        fs::symlink_metadata(path).unwrap().permissions().readonly()
    }

    /// A read-only file is unlinked as any other (`IGNORE_READONLY_ATTRIBUTE`).
    #[test]
    fn a_read_only_file_is_unlinked() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join(".ddc-keep"), b"").unwrap();
        read_only(&d.path().join(".ddc-keep"));
        DirFd::open(d.path()).unwrap().unlink_child(OsStr::new(".ddc-keep")).unwrap();
        assert!(!d.path().join(".ddc-keep").exists());
    }

    /// The plain disposition, which FAT and exFAT fall back to, is refused
    /// for a read-only file on NTFS too: the fallback clears the attribute
    /// on the same file and deletes it.
    #[test]
    fn the_plain_fallback_clears_read_only_and_deletes() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("f"), b"x").unwrap();
        read_only(&d.path().join("f"));
        let dir = DirFd::open(d.path()).unwrap();
        let child = dir.child(OsStr::new("f"), DELETE | SYNCHRONIZE, FILE_NON_DIRECTORY_FILE).unwrap();
        let plain = set_info(&child, FileDispositionInfo, &FILE_DISPOSITION_INFO { DeleteFile: true });
        assert!(plain.as_ref().is_err_and(denied), "{plain:?}");
        DirFd::unlink_plain(&child).unwrap();
        drop(child);
        assert!(!d.path().join("f").exists());
    }

    /// FAT and exFAT by the names the volume gives; anything else is left
    /// alone, so on NTFS the clearing changes nothing.
    #[test]
    fn only_fat_and_exfat_have_their_read_only_files_cleared() {
        for fat in ["fat", "fat32", "exfat"] {
            assert!(is_fat(fat), "{fat}");
        }
        for other in ["ntfs", "refs", "", "fat64"] {
            assert!(!is_fat(other), "{other}");
        }
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("kept"), b"x").unwrap();
        read_only(&d.path().join("kept"));
        clear_read_only(d.path()).unwrap();
        assert!(is_read_only(&d.path().join("kept")), "NTFS needs no clearing");
    }

    /// The walk clears every plain file below the root, in sub-folders too,
    /// and never through a link: a junction's target and a symlink's
    /// target keep their attribute.
    #[test]
    fn clearing_reaches_every_file_below_and_never_follows_a_link() {
        let d = tempfile::tempdir().unwrap();
        let (root, outside) = (d.path().join("root"), d.path().join("outside"));
        fs::create_dir_all(root.join("sub/deeper")).unwrap();
        fs::create_dir(&outside).unwrap();
        for f in [root.join("a"), root.join("sub/deeper/b"), outside.join("c")] {
            fs::write(&f, b"x").unwrap();
            read_only(&f);
        }
        junction(&root.join("j"), &outside);
        let symlink = std::os::windows::fs::symlink_file(outside.join("c"), root.join("l")).is_ok();

        clear_read_only_below(&root).unwrap();

        assert!(!is_read_only(&root.join("a")) && !is_read_only(&root.join("sub/deeper/b")));
        assert!(is_read_only(&outside.join("c")), "nothing behind a link is changed (symlink made: {symlink})");
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
