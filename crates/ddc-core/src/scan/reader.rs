//! Directory listing abstraction. [`SystemReader`] (the platform's listing:
//! `getattrlistbulk` on macOS) is the fast path; [`StdReader`] uses `std::fs` and serves as a reference in
//! tests and as a fallback.

use std::fs;
use std::os::macos::fs::MetadataExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;

pub use crate::platform::types::{DirListing, EntryKind, EntryMeta, ReadError, ReadErrorKind};

/// `SF_DATALESS` from `<sys/stat.h>`: the item's content lives in the cloud.
pub const SF_DATALESS: u32 = 0x4000_0000;

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

/// The platform's own listing (`platform::bulk::list`): on macOS
/// `getattrlistbulk`, one call per batch of entries, with the APFS private
/// size and the date added.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemReader;

impl DirReader for SystemReader {
    fn read(&self, dir: &Path, expect: Option<(u64, u64)>, want_private: bool) -> Result<DirListing, ReadError> {
        crate::platform::bulk::list(dir, expect, want_private, false)
    }

    fn read_with_times(
        &self,
        dir: &Path,
        expect: Option<(u64, u64)>,
        want_private: bool,
    ) -> Result<DirListing, ReadError> {
        crate::platform::bulk::list(dir, expect, want_private, true)
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
    for entry in crate::stats::read_dir(dir).map_err(|e| ReadError::from_io(&e))? {
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
                    error: Some(e.raw_os_error().unwrap_or(crate::platform::errno::EIO)),
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
