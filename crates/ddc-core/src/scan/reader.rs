//! Directory listing abstraction. [`SystemReader`] (the platform's listing:
//! `getattrlistbulk` on macOS) is the fast path; [`StdReader`] uses `std::fs` and serves as a reference in
//! tests and as a fallback.

use std::fs;
use std::path::Path;

use crate::platform::meta;
pub use crate::platform::types::{DirListing, EntryKind, EntryMeta, ReadError, ReadErrorKind, Times};

/// `SF_DATALESS` from `<sys/stat.h>`: the item's content lives in the cloud.
pub const SF_DATALESS: u32 = 0x4000_0000;

pub trait DirReader: Sync {
    /// Lists `dir` without following symlinks. `expect` is the `(dev, ino)` the
    /// parent listing reported; if the opened directory differs, nothing is read.
    /// `want_private` asks for [`EntryMeta::private`], `times` for
    /// [`EntryMeta::modified`] and [`EntryMeta::added`]: a walk asks for them
    /// only inside a claim that shows a date and in a user-data folder's own
    /// listing, since every record pays for them. A reader that cannot tell
    /// leaves them `None`.
    fn read(
        &self,
        dir: &Path,
        expect: Option<(u64, u64)>,
        want_private: bool,
        times: Times,
    ) -> Result<DirListing, ReadError>;
}

/// The platform's own listing (`platform::bulk::list`): on macOS
/// `getattrlistbulk`, one call per batch of entries, with the APFS private
/// size and the date added.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemReader;

impl DirReader for SystemReader {
    fn read(
        &self,
        dir: &Path,
        expect: Option<(u64, u64)>,
        want_private: bool,
        times: Times,
    ) -> Result<DirListing, ReadError> {
        crate::platform::bulk::list(dir, expect, want_private, times)
    }
}

/// Reference reader on `std::fs` (`readdir` + `lstat` per entry). No private
/// size, and no date added: `std` has none.
#[derive(Debug, Default, Clone, Copy)]
pub struct StdReader;

impl DirReader for StdReader {
    fn read(
        &self,
        dir: &Path,
        expect: Option<(u64, u64)>,
        _want_private: bool,
        times: Times,
    ) -> Result<DirListing, ReadError> {
        list(dir, expect, times != Times::None)
    }
}

fn list(dir: &Path, expect: Option<(u64, u64)>, times: bool) -> Result<DirListing, ReadError> {
    let md = fs::symlink_metadata(dir).map_err(|e| ReadError::from_io(&e))?;
    if !md.is_dir() {
        return Err(ReadError::new(ReadErrorKind::Changed, None));
    }
    let (dev, ino) = meta::file_id(&md).map_err(|e| ReadError::from_io(&e))?;
    let modified = Some(meta::mtime(&md));
    if let Some((want_dev, want_ino)) = expect {
        if dev != want_dev {
            return Err(ReadError::new(ReadErrorKind::OtherFilesystem, None));
        }
        if ino != want_ino {
            return Err(ReadError::new(ReadErrorKind::Changed, None));
        }
    }
    let mut entries = Vec::new();
    for entry in crate::stats::read_dir(dir).map_err(|e| ReadError::from_io(&e))? {
        let entry = entry.map_err(|e| ReadError::from_io(&e))?;
        // `DirEntry::metadata` is `fstatat(.., AT_SYMLINK_NOFOLLOW)` on macOS.
        let (em, (edev, eino)) = match entry.metadata().and_then(|m| meta::file_id(&m).map(|id| (m, id))) {
            Ok(found) => found,
            Err(e) => {
                entries.push(EntryMeta {
                    name: entry.file_name(),
                    kind: EntryKind::Other,
                    dev: 0,
                    ino: 0,
                    nlink: 1,
                    uid: None,
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
        let ft = em.file_type();
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
            dev: edev,
            ino: eino,
            nlink: u32::try_from(meta::link_count(&em)).unwrap_or(u32::MAX),
            uid: meta::owner(&em),
            flags: meta::st_flags(&em),
            alloc: meta::alloc_bytes(&em),
            private: None,
            error: None,
            modified: times.then(|| meta::mtime(&em)),
            added: None,
        });
    }
    Ok(DirListing { dev, ino, modified, alloc: meta::alloc_bytes(&md), entries })
}
