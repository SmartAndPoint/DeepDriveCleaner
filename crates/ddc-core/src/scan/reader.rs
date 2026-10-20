//! Directory listing abstraction. [`BulkReader`](crate::sys::bulk::BulkReader)
//! is the fast path; [`StdReader`] uses `std::fs` and serves as a reference in
//! tests and as a fallback.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::os::macos::fs::MetadataExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

/// `SF_DATALESS` from `<sys/stat.h>`: the item's content lives in the cloud.
pub const SF_DATALESS: u32 = 0x4000_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
    Other,
}

/// One directory entry, never following symlinks.
#[derive(Debug, Clone)]
pub struct EntryMeta {
    pub name: OsString,
    pub kind: EntryKind,
    pub dev: u64,
    pub ino: u64,
    pub nlink: u32,
    /// `st_flags` (`SF_DATALESS`, …).
    pub flags: u32,
    /// Allocated bytes on disk. Directories report 0 (as on APFS).
    pub alloc: u64,
    /// APFS private size: bytes freed immediately if this file alone were
    /// deleted. `None` when not requested or not supported.
    pub private: Option<u64>,
    /// Per-entry errno reported by the filesystem; other fields may be unset.
    pub error: Option<i32>,
    /// Modification time, Unix seconds. Only from [`DirReader::read_with_times`].
    pub modified: Option<i64>,
    /// When the entry was added to this directory (`ATTR_CMN_ADDEDTIME`),
    /// Unix seconds. Only from [`DirReader::read_with_times`], where the
    /// filesystem keeps it.
    pub added: Option<i64>,
}

/// Entries of one directory plus the identity of the directory itself.
#[derive(Debug, Clone)]
pub struct DirListing {
    pub dev: u64,
    pub ino: u64,
    pub entries: Vec<EntryMeta>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ReadErrorKind {
    PermissionDenied,
    NotFound,
    /// Evicted cloud item with materialization off (`EDEADLK`).
    Dataless,
    /// The path no longer is the directory the parent listed (symlink swap, replaced).
    Changed,
    /// A different filesystem is mounted here.
    OtherFilesystem,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadError {
    pub kind: ReadErrorKind,
    pub errno: Option<i32>,
}

impl ReadError {
    pub fn new(kind: ReadErrorKind, errno: Option<i32>) -> Self {
        Self { kind, errno }
    }

    pub fn from_errno(errno: i32) -> Self {
        let kind = match errno {
            libc::EACCES | libc::EPERM => ReadErrorKind::PermissionDenied,
            libc::ENOENT => ReadErrorKind::NotFound,
            libc::EDEADLK => ReadErrorKind::Dataless,
            libc::ELOOP | libc::ENOTDIR => ReadErrorKind::Changed,
            _ => ReadErrorKind::Other,
        };
        Self { kind, errno: Some(errno) }
    }

    fn from_io(err: &io::Error) -> Self {
        Self::from_errno(err.raw_os_error().unwrap_or(libc::EIO))
    }
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.errno {
            Some(errno) => write!(f, "{:?}: {}", self.kind, io::Error::from_raw_os_error(errno)),
            None => write!(f, "{:?}", self.kind),
        }
    }
}

impl std::error::Error for ReadError {}

pub trait DirReader: Sync {
    /// Lists `dir` without following symlinks. `expect` is the `(dev, ino)` the
    /// parent listing reported; if the opened directory differs, nothing is read.
    /// `want_private` asks for [`EntryMeta::private`].
    fn read(&self, dir: &Path, expect: Option<(u64, u64)>, want_private: bool) -> Result<DirListing, ReadError>;

    /// [`read`](Self::read) with each entry's [`EntryMeta::modified`] and
    /// [`EntryMeta::added`]. Asked only for a user-data folder's own listing
    /// (story "User data: pick your own files for the Trash"): every other
    /// directory of a walk would pay for them. A reader that cannot tell
    /// leaves them `None`.
    fn read_with_times(
        &self,
        dir: &Path,
        expect: Option<(u64, u64)>,
        want_private: bool,
    ) -> Result<DirListing, ReadError> {
        self.read(dir, expect, want_private)
    }
}

/// Reference reader on `std::fs` (`readdir` + `lstat` per entry). No private size.
#[derive(Debug, Default, Clone, Copy)]
pub struct StdReader;

impl DirReader for StdReader {
    fn read(&self, dir: &Path, expect: Option<(u64, u64)>, _want_private: bool) -> Result<DirListing, ReadError> {
        list(dir, expect, false)
    }

    /// The modification time only: `std` has no date added.
    fn read_with_times(
        &self,
        dir: &Path,
        expect: Option<(u64, u64)>,
        _want_private: bool,
    ) -> Result<DirListing, ReadError> {
        list(dir, expect, true)
    }
}

fn list(dir: &Path, expect: Option<(u64, u64)>, times: bool) -> Result<DirListing, ReadError> {
    let md = fs::symlink_metadata(dir).map_err(|e| ReadError::from_io(&e))?;
    if !md.is_dir() {
        return Err(ReadError::new(ReadErrorKind::Changed, None));
    }
    if let Some((want_dev, want_ino)) = expect {
        if md.dev() != want_dev {
            return Err(ReadError::new(ReadErrorKind::OtherFilesystem, None));
        }
        if md.ino() != want_ino {
            return Err(ReadError::new(ReadErrorKind::Changed, None));
        }
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| ReadError::from_io(&e))? {
        let entry = entry.map_err(|e| ReadError::from_io(&e))?;
        // `DirEntry::metadata` is `fstatat(.., AT_SYMLINK_NOFOLLOW)` on macOS.
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(e) => {
                entries.push(EntryMeta {
                    name: entry.file_name(),
                    kind: EntryKind::Other,
                    dev: 0,
                    ino: 0,
                    nlink: 1,
                    flags: 0,
                    alloc: 0,
                    private: None,
                    error: Some(e.raw_os_error().unwrap_or(libc::EIO)),
                    modified: None,
                    added: None,
                });
                continue;
            }
        };
        let ft = meta.file_type();
        let kind = if ft.is_dir() {
            EntryKind::Dir
        } else if ft.is_file() {
            EntryKind::File
        } else if ft.is_symlink() {
            EntryKind::Symlink
        } else {
            EntryKind::Other
        };
        entries.push(EntryMeta {
            name: entry.file_name(),
            kind,
            dev: meta.dev(),
            ino: meta.ino(),
            nlink: u32::try_from(meta.nlink()).unwrap_or(u32::MAX),
            flags: meta.st_flags(),
            alloc: if kind == EntryKind::Dir { 0 } else { meta.blocks() * 512 },
            private: None,
            error: None,
            modified: times.then(|| meta.mtime()),
            added: None,
        });
    }
    Ok(DirListing { dev: md.dev(), ino: md.ino(), entries })
}
