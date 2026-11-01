//! Directory reader on Windows (story "Windows scanner and rule catalog",
//! step 6): the folder is opened by its own handle (`fsops::DirFd`, never
//! through a link), and its entries are read in 64 KiB batches with
//! `FileIdExtdDirectoryInfo`, which gives per entry the name, attributes,
//! allocated size, times, reparse tag and 128-bit file id in one call, the
//! counterpart of `getattrlistbulk`. A volume without it (FAT, exFAT) gets
//! `FileFullDirectoryInfo` and one relative open per entry for its id.
//!
//! - **Links.** An entry is a link when its reparse tag is a name surrogate
//!   (symlink, junction, WSL symlink); it is reported as a link and never
//!   descended. A mount-point tag is read once: a volume mount point (its
//!   target is `\??\Volume{…}`) is another file system, a [`Boundary`] the
//!   walk does not open, so a finding holding one is incomplete; any other
//!   target is a junction, a link. Cloud placeholders, WOF-compressed and
//!   other tagged files are files or folders as their attributes say, and
//!   nothing here opens them, so no placeholder is downloaded.
//! - **Hard links.** The listing carries no link count. A regular file with
//!   allocated bytes reports `nlink` 0, "unknown", and the walk's deferred
//!   inode table counts it once across every place it is seen (pnpm and uv
//!   hard-link almost every file on Windows). A link whose other name lies
//!   outside the scan is counted as freed: a size overstated, never a
//!   deletion widened.
//! - **One volume.** [`list_in`] opens a folder by path and checks that it is
//!   on its scan root's volume; junctions never lead there, since they are
//!   never descended, and a mount point is a boundary.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::OsStringExt as _;
use std::os::windows::io::AsRawHandle as _;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{
    ERROR_INVALID_LEVEL, ERROR_INVALID_PARAMETER, ERROR_NO_MORE_FILES, ERROR_NOT_SUPPORTED,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_BASIC_INFO, FILE_FULL_DIR_INFO, FILE_ID_EXTD_DIR_INFO,
    FILE_ID_INFO, FILE_READ_ATTRIBUTES, FILE_STANDARD_INFO, FileFullDirectoryInfo, FileFullDirectoryRestartInfo,
    FileIdExtdDirectoryInfo, FileIdExtdDirectoryRestartInfo, GetFileInformationByHandleEx, SYNCHRONIZE,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

use super::fsops::{DirFd, name_surrogate, unix_seconds};
use super::meta::{id_of, info};
use crate::platform::types::{Boundary, DirListing, EntryKind, EntryMeta, ReadError, ReadErrorKind, Times};

const BUF_LEN: usize = 64 * 1024;
/// `IO_REPARSE_TAG_MOUNT_POINT`: a junction or a volume mount point.
const MOUNT_POINT: u32 = 0xA000_0003;
/// `FSCTL_GET_REPARSE_POINT`.
const GET_REPARSE_POINT: u32 = 0x0009_00A8;

/// A scan root, which [`list_in`] keeps folders on the volume of.
#[derive(Debug)]
pub struct Anchor {
    path: PathBuf,
    dev: u64,
}

/// Opens `root` (canonical) as the anchor of a walk.
pub fn anchor(root: &Path) -> io::Result<Anchor> {
    let dir = DirFd::open(root)?;
    let (dev, _) = dir.identity()?;
    Ok(Anchor { path: root.to_path_buf(), dev })
}

/// [`list`] of `dir`, a folder at or beneath `anchor`'s root and on its
/// volume.
pub fn list_in(
    anchor: &Anchor,
    dir: &Path,
    expect: Option<(u64, u64)>,
    want_private: bool,
    times: Times,
) -> Result<DirListing, ReadError> {
    if !dir.starts_with(&anchor.path) {
        return Err(ReadError::new(ReadErrorKind::Changed, None));
    }
    let listing = list(dir, expect, want_private, times)?;
    if listing.dev != anchor.dev {
        return Err(ReadError::new(ReadErrorKind::OtherFilesystem, None));
    }
    Ok(listing)
}

/// Lists `dir`, opened by its own handle and never through a link.
/// `expect` is checked before any entry is read; `want_private` is ignored
/// (Windows keeps no clone-aware private size).
pub fn list(
    dir: &Path,
    expect: Option<(u64, u64)>,
    _want_private: bool,
    times: Times,
) -> Result<DirListing, ReadError> {
    let folder = DirFd::open(dir).map_err(|e| read_error(&e))?;
    let (dev, ino) =
        id_of(&info::<FILE_ID_INFO>(folder.handle()).map_err(|e| read_error(&e))?).map_err(|e| read_error(&e))?;
    if let Some((want_dev, want_ino)) = expect {
        if dev != want_dev {
            return Err(ReadError::new(ReadErrorKind::OtherFilesystem, None));
        }
        if ino != want_ino {
            return Err(ReadError::new(ReadErrorKind::Changed, None));
        }
    }
    let basic = info::<FILE_BASIC_INFO>(folder.handle()).map_err(|e| read_error(&e))?;
    let standard = info::<FILE_STANDARD_INFO>(folder.handle()).map_err(|e| read_error(&e))?;
    let mut entries = Vec::new();
    match read_all(
        &folder,
        FileIdExtdDirectoryRestartInfo,
        FileIdExtdDirectoryInfo,
        size_of::<FILE_ID_EXTD_DIR_INFO>(),
        |record| {
            // SAFETY (of the read): `record` starts a FILE_ID_EXTD_DIR_INFO the
            // call wrote, and `read_all` checked its fixed part is left; read
            // unaligned.
            let header = unsafe { std::ptr::read_unaligned(record.as_ptr().cast::<FILE_ID_EXTD_DIR_INFO>()) };
            let name = name_of(record, std::mem::offset_of!(FILE_ID_EXTD_DIR_INFO, FileName), header.FileNameLength);
            let id = id_of(&FILE_ID_INFO { VolumeSerialNumber: dev, FileId: header.FileId });
            Raw {
                name,
                attributes: header.FileAttributes,
                tag: header.ReparsePointTag,
                alloc: header.AllocationSize,
                modified: header.LastWriteTime,
                created: header.CreationTime,
                id: Some(id),
            }
        },
    ) {
        Ok(raws) => entries.extend(raws.into_iter().filter_map(|raw| entry(&folder, raw, dev, times))),
        Err(e) if refused_class(&e) => {
            let raws = read_all(
                &folder,
                FileFullDirectoryRestartInfo,
                FileFullDirectoryInfo,
                size_of::<FILE_FULL_DIR_INFO>(),
                |record| {
                    // SAFETY (of the read): as above, for FILE_FULL_DIR_INFO.
                    let header = unsafe { std::ptr::read_unaligned(record.as_ptr().cast::<FILE_FULL_DIR_INFO>()) };
                    let name =
                        name_of(record, std::mem::offset_of!(FILE_FULL_DIR_INFO, FileName), header.FileNameLength);
                    Raw {
                        name,
                        attributes: header.FileAttributes,
                        // `EaSize` carries the reparse tag when the entry is a
                        // reparse point (it is `WIN32_FIND_DATA.dwReserved0`).
                        tag: if header.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 { header.EaSize } else { 0 },
                        alloc: header.AllocationSize,
                        modified: header.LastWriteTime,
                        created: header.CreationTime,
                        id: None,
                    }
                },
            )
            .map_err(|e| read_error(&e))?;
            entries.extend(raws.into_iter().filter_map(|raw| entry(&folder, raw, dev, times)));
        }
        Err(e) => return Err(read_error(&e)),
    }
    // Counted once the listing is whole; a listing that fails part-way counts nothing.
    crate::stats::add(crate::stats::Counter::DirEntriesRead, entries.len() as u64);
    Ok(DirListing {
        dev,
        ino,
        modified: Some(unix_seconds(basic.LastWriteTime)),
        alloc: u64::try_from(standard.AllocationSize).unwrap_or(0),
        entries,
    })
}

/// One record of a listing, before it becomes an entry.
struct Raw {
    name: OsString,
    attributes: u32,
    tag: u32,
    alloc: i64,
    modified: i64,
    created: i64,
    /// From the listing; `None` where the class carries no id (FAT).
    id: Option<io::Result<(u64, u64)>>,
}

fn entry(folder: &DirFd, raw: Raw, dev: u64, times: Times) -> Option<EntryMeta> {
    if raw.name == OsStr::new(".") || raw.name == OsStr::new("..") {
        return None;
    }
    let (mut kind, mut boundary) = if name_surrogate(raw.attributes, raw.tag) {
        (EntryKind::Symlink, None)
    } else if raw.attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        (EntryKind::Dir, None)
    } else {
        (EntryKind::File, None)
    };
    if kind == EntryKind::Symlink && raw.tag == MOUNT_POINT && raw.attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        // A junction is a link; a volume mount point is another file
        // system. Unreadable: the cautious answer, a boundary.
        if !matches!(junction_or_volume(folder, &raw.name), Some(false)) {
            kind = EntryKind::Dir;
            boundary = Some(Boundary::Mount);
        }
    }
    let id = match raw.id {
        Some(id) => id,
        None => folder.stat_child(&raw.name).map(|st| (st.dev, st.ino)),
    };
    let alloc = u64::try_from(raw.alloc).unwrap_or(0);
    let mut meta = EntryMeta {
        name: raw.name,
        kind,
        dev,
        ino: 0,
        // Unknown for a file with allocated bytes: the walk's inode table
        // counts it once wherever its links are seen.
        nlink: if kind == EntryKind::File && alloc > 0 { 0 } else { 1 },
        uid: None,
        flags: 0,
        alloc,
        private: None,
        error: None,
        modified: (times != Times::None).then(|| unix_seconds(raw.modified)),
        added: (times == Times::ModifiedAndAdded).then(|| unix_seconds(raw.created)),
        boundary,
    };
    match id {
        Ok((entry_dev, ino)) => {
            meta.dev = entry_dev;
            meta.ino = ino;
        }
        Err(e) => {
            meta.kind = EntryKind::Other;
            meta.error = Some(e.raw_os_error().unwrap_or(super::errno::EIO));
        }
    }
    Some(meta)
}

/// Whether the mount point `name` is a junction (`Some(false)`: a link) or
/// a volume mount point (`Some(true)`); `None` when its target cannot be read.
fn junction_or_volume(folder: &DirFd, name: &OsStr) -> Option<bool> {
    let child = folder.child(name, FILE_READ_ATTRIBUTES | SYNCHRONIZE, 0).ok()?;
    let mut buffer = vec![0u64; 16 * 1024 / 8];
    let mut returned = 0u32;
    // SAFETY: the handle is open for the call; the output buffer is a
    // writable local of the stated size; there is no input buffer and no
    // overlapped structure (a synchronous handle).
    let ok = unsafe {
        DeviceIoControl(
            child.as_raw_handle(),
            GET_REPARSE_POINT,
            std::ptr::null(),
            0,
            buffer.as_mut_ptr().cast(),
            u32::try_from(buffer.len() * 8).ok()?,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return None;
    }
    let bytes: Vec<u8> = buffer.iter().flat_map(|w| w.to_le_bytes()).take(returned as usize).collect();
    mount_target_is_volume(&bytes)
}

/// From a mount point's `REPARSE_DATA_BUFFER`: whether its target is a
/// volume (`\??\Volume{…}`). The buffer is the tag (4 bytes), the data
/// length (2), reserved (2), then the substitute name's offset and length
/// and the print name's (2 each), then the path buffer at byte 16, which the
/// offsets count from. `None` for a buffer too short for what it claims.
fn mount_target_is_volume(bytes: &[u8]) -> Option<bool> {
    let u16_at = |at: usize| bytes.get(at..at + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let (offset, length) = (usize::from(u16_at(8)?), usize::from(u16_at(10)?));
    let start = 16 + offset;
    let units: Vec<u16> =
        bytes.get(start..start + length)?.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect();
    let target = String::from_utf16_lossy(&units).to_ascii_lowercase();
    Some(target.starts_with(r"\??\volume{"))
}

/// Every record of the folder through one information class, restarting at
/// its first entry; `parse` turns each record (from its start to the end of
/// the batch) into a [`Raw`].
fn read_all(
    folder: &DirFd,
    restart: i32,
    next: i32,
    fixed: usize,
    parse: impl Fn(&[u8]) -> Raw,
) -> io::Result<Vec<Raw>> {
    let mut out = Vec::new();
    let mut buffer = vec![0u64; BUF_LEN / 8];
    let mut class = restart;
    loop {
        // SAFETY: the buffer is a writable, 8-aligned local of BUF_LEN bytes
        // and the folder's handle is open for the call.
        let ok = unsafe {
            GetFileInformationByHandleEx(
                folder.handle().as_raw_handle(),
                class,
                buffer.as_mut_ptr().cast(),
                BUF_LEN as u32,
            )
        };
        if ok == 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                return Ok(out);
            }
            return Err(e);
        }
        class = next;
        let bytes: &[u8] =
            // SAFETY: the buffer holds BUF_LEN initialized bytes (zeroed,
            // then written by the call); viewed as bytes for the walk.
            unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), BUF_LEN) };
        let mut at = 0usize;
        loop {
            let record = &bytes[at..];
            // `parse` reads the record's whole fixed part: stop where less
            // than that is left (never, for records the call wrote).
            if record.len() < fixed {
                break;
            }
            out.push(parse(record));
            let next_offset = u32::from_le_bytes([record[0], record[1], record[2], record[3]]) as usize;
            if next_offset == 0 {
                break;
            }
            at += next_offset;
            if at >= BUF_LEN {
                break;
            }
        }
    }
}

/// The name of a record: `length` bytes from `offset`, UTF-16, unaligned.
fn name_of(record: &[u8], offset: usize, length: u32) -> OsString {
    let end = (offset + length as usize).min(record.len());
    let units: Vec<u16> =
        record.get(offset..end).unwrap_or(&[]).as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect();
    OsString::from_wide(&units)
}

/// The volume does not know the extended class (FAT, exFAT, some network
/// shares).
fn refused_class(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(c) if c == ERROR_INVALID_PARAMETER as i32 || c == ERROR_INVALID_LEVEL as i32 || c == ERROR_NOT_SUPPORTED as i32
    )
}

fn read_error(e: &io::Error) -> ReadError {
    let code = e.raw_os_error();
    ReadError::new(code.map_or(ReadErrorKind::Other, super::errno::read_error_kind), code)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::platform::meta;
    use crate::scan::reader::{DirReader, StdReader};

    fn junction(link: &Path, target: &Path) {
        let made =
            std::process::Command::new("cmd").arg("/C").arg("mklink").arg("/J").arg(link).arg(target).output().unwrap();
        assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
    }

    fn by_name(l: &DirListing, name: &str) -> EntryMeta {
        l.entries.iter().find(|e| e.name == name).unwrap_or_else(|| panic!("{name}")).clone()
    }

    #[test]
    fn a_listing_agrees_with_meta_on_every_entry() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("f"), vec![1u8; 9000]).unwrap();
        fs::write(d.path().join("empty"), b"").unwrap();
        fs::create_dir(d.path().join("sub")).unwrap();
        let l = list(d.path(), None, false, Times::Modified).unwrap();
        assert_eq!((l.dev, l.ino), meta::lstat(d.path()).unwrap().id());
        assert_eq!(l.entries.len(), 3, "no . or ..");
        let f = by_name(&l, "f");
        let st = meta::lstat(&d.path().join("f")).unwrap();
        assert_eq!((f.kind, f.dev, f.ino, f.alloc), (EntryKind::File, st.dev, st.ino, st.alloc));
        assert_eq!(f.nlink, 0, "a file with allocated bytes: links unknown");
        assert!(f.modified.is_some() && f.added.is_none());
        assert_eq!(by_name(&l, "empty").nlink, 1);
        assert_eq!(by_name(&l, "sub").kind, EntryKind::Dir);
    }

    #[test]
    fn a_junction_is_a_link_and_never_followed() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret"), b"x").unwrap();
        junction(&d.path().join("j"), outside.path());
        let l = list(d.path(), None, false, Times::None).unwrap();
        let j = by_name(&l, "j");
        assert_eq!(j.kind, EntryKind::Symlink);
        assert!(j.boundary.is_none());
        assert_ne!(j.ino, meta::lstat(outside.path()).unwrap().ino);
    }

    #[test]
    fn an_expected_identity_that_changed_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let (dev, ino) = meta::lstat(d.path()).unwrap().id();
        assert!(list(d.path(), Some((dev, ino)), false, Times::None).is_ok());
        assert_eq!(list(d.path(), Some((dev, ino + 1)), false, Times::None).unwrap_err().kind, ReadErrorKind::Changed);
        assert_eq!(
            list(d.path(), Some((dev ^ 1, ino)), false, Times::None).unwrap_err().kind,
            ReadErrorKind::OtherFilesystem
        );
        let j = d.path().join("j");
        junction(&j, d.path());
        assert_eq!(
            list(&j, None, false, Times::None).unwrap_err().kind,
            ReadErrorKind::Changed,
            "a link is not opened"
        );
    }

    #[test]
    fn many_entries_take_more_than_one_batch_and_match_the_std_reader() {
        let d = tempfile::tempdir().unwrap();
        for i in 0..2000 {
            fs::write(d.path().join(format!("a-reasonably-long-file-name-{i:05}.txt")), b"x").unwrap();
        }
        let ours = list(d.path(), None, false, Times::None).unwrap();
        let reference = StdReader.read(d.path(), None, false, Times::None).unwrap();
        let mut a: Vec<_> = ours.entries.iter().map(|e| (e.name.clone(), e.ino)).collect();
        let mut b: Vec<_> = reference.entries.iter().map(|e| (e.name.clone(), e.ino)).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b);
    }

    #[test]
    fn a_name_longer_than_max_path_is_listed_and_times_are_unix_seconds() {
        let d = tempfile::tempdir().unwrap();
        let mut deep = d.path().to_path_buf();
        while deep.as_os_str().len() < 300 {
            deep.push("a-forty-character-folder-name-to-go-deep");
        }
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("f"), b"x").unwrap();
        let l = list(&deep, None, false, Times::ModifiedAndAdded).unwrap();
        let f = by_name(&l, "f");
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
        assert!((now - f.modified.unwrap()).abs() < 120 && (now - f.added.unwrap()).abs() < 120);
    }

    #[test]
    fn hard_links_report_one_identity() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a"), vec![1u8; 5000]).unwrap();
        fs::hard_link(d.path().join("a"), d.path().join("b")).unwrap();
        let l = list(d.path(), None, false, Times::None).unwrap();
        let (a, b) = (by_name(&l, "a"), by_name(&l, "b"));
        assert_eq!((a.dev, a.ino), (b.dev, b.ino));
        assert_eq!((a.nlink, b.nlink), (0, 0));
    }

    fn reparse(target: &str) -> Vec<u8> {
        let units: Vec<u8> = target.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut b = Vec::new();
        b.extend(MOUNT_POINT.to_le_bytes());
        b.extend(u16::try_from(8 + units.len() + 4).unwrap().to_le_bytes());
        b.extend([0u8; 2]);
        b.extend(0u16.to_le_bytes()); // substitute offset
        b.extend(u16::try_from(units.len()).unwrap().to_le_bytes());
        b.extend(u16::try_from(units.len() + 2).unwrap().to_le_bytes()); // print offset
        b.extend(0u16.to_le_bytes());
        b.extend(&units);
        b.extend([0u8; 4]);
        b
    }

    #[test]
    fn a_mount_point_is_a_volume_only_by_a_volume_target() {
        assert_eq!(mount_target_is_volume(&reparse(r"\??\Volume{1b2c3d4e-0000-0000-0000-100000000000}\")), Some(true));
        assert_eq!(mount_target_is_volume(&reparse(r"\??\C:\Users\u\target")), Some(false));
        assert_eq!(mount_target_is_volume(&reparse(r"\??\volume{lower-case}\")), Some(true));
        let whole = reparse(r"\??\Volume{x}\");
        assert_eq!(mount_target_is_volume(&whole[..20]), None, "a truncated buffer");
        assert_eq!(mount_target_is_volume(&[0u8; 6]), None, "too short");
    }

    #[test]
    fn a_compressed_file_is_a_file() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("big.txt");
        fs::write(&f, "compressible line of text\n".repeat(20_000)).unwrap();
        let Ok(out) = std::process::Command::new("compact").arg("/c").arg("/exe:xpress4k").arg(&f).output() else {
            return;
        };
        if !out.status.success() {
            return;
        }
        let l = list(d.path(), None, false, Times::None).unwrap();
        assert_eq!(by_name(&l, "big.txt").kind, EntryKind::File);
    }
}
