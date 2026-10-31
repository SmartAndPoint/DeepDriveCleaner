//! Directory reader on `getdents64(2)` and `statx(2)`: the directory is
//! opened without following a symlink, read in batches of names, and each
//! entry is stat'ed relative to the open directory, never following a
//! symlink. Linux has no bulk stat, so it is one `statx` per entry. No
//! private size: Linux keeps none. Linux keeps no date added either; the
//! birth time stands in for it where the filesystem records one (ext4,
//! btrfs, XFS), so a download's date is when it was written.

use std::ffi::{CStr, OsStr};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;

use rustix::fs::{AtFlags, FileType, Mode, OFlags, RawDir, StatxFlags};
use rustix::io::Errno;

use crate::platform::types::{DirListing, EntryKind, EntryMeta, ReadError, ReadErrorKind, Times};

const BUF_LEN: usize = 64 * 1024;

/// What one `statx` (or the `fstatat` fallback) says of an entry.
struct Stat {
    kind: EntryKind,
    dev: u64,
    ino: u64,
    nlink: u32,
    uid: u32,
    blocks: u64,
    mtime: i64,
    /// Asked for only with [`Times::ModifiedAndAdded`], and `None` where the
    /// filesystem keeps no birth time.
    btime: Option<i64>,
}

pub fn list(
    dir: &Path,
    expect: Option<(u64, u64)>,
    _want_private: bool,
    times: Times,
) -> Result<DirListing, ReadError> {
    let fd = retry(|| {
        rustix::fs::open(dir, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())
    })
    .map_err(read_error)?;
    let me = stat(&fd, c"", AtFlags::EMPTY_PATH, false).map_err(read_error)?;
    if let Some((want_dev, want_ino)) = expect {
        if me.dev != want_dev {
            return Err(ReadError::new(ReadErrorKind::OtherFilesystem, None));
        }
        if me.ino != want_ino {
            return Err(ReadError::new(ReadErrorKind::Changed, None));
        }
    }

    let mut entries = Vec::new();
    let mut buf = vec![MaybeUninit::<u8>::uninit(); BUF_LEN];
    let mut names = RawDir::new(&fd, &mut buf);
    while let Some(entry) = names.next() {
        let entry = match entry {
            Ok(entry) => entry,
            Err(Errno::INTR) => continue,
            Err(e) => return Err(read_error(e)),
        };
        let name = entry.file_name();
        if name == c"." || name == c".." {
            continue;
        }
        entries.push(entry_meta(&fd, name, times));
    }
    // Counted once the listing is whole; a listing that fails part-way counts nothing.
    crate::stats::add(crate::stats::Counter::DirEntriesRead, entries.len() as u64);
    Ok(DirListing { dev: me.dev, ino: me.ino, modified: Some(me.mtime), alloc: me.blocks * 512, entries })
}

/// One entry, stat'ed relative to `dir` without following a symlink. An
/// entry that cannot be stat'ed (gone since the listing, or refused) carries
/// its errno, as the reference reader's does.
fn entry_meta(dir: &OwnedFd, name: &CStr, times: Times) -> EntryMeta {
    let name_os = OsStr::from_bytes(name.to_bytes()).to_owned();
    let added = times == Times::ModifiedAndAdded;
    match stat(dir, name, AtFlags::SYMLINK_NOFOLLOW | AtFlags::STATX_DONT_SYNC, added) {
        Ok(st) => EntryMeta {
            name: name_os,
            kind: st.kind,
            dev: st.dev,
            ino: st.ino,
            nlink: st.nlink,
            uid: Some(st.uid),
            flags: 0,
            alloc: st.blocks * 512,
            private: None,
            error: None,
            modified: (times != Times::None).then_some(st.mtime),
            added: st.btime,
        },
        Err(e) => EntryMeta {
            name: name_os,
            kind: EntryKind::Other,
            dev: 0,
            ino: 0,
            nlink: 1,
            uid: None,
            flags: 0,
            alloc: 0,
            private: None,
            error: Some(e.raw_os_error()),
            modified: None,
            added: None,
        },
    }
}

/// `statx`, or `fstatat` where the kernel or a seccomp filter refuses
/// `statx` (`ENOSYS`, `EPERM`). `dev` is `makedev` of the major and minor
/// numbers, which is what std's `MetadataExt::dev` gives. The birth time
/// only when `btime` asks for it and `statx` returns it.
fn stat(dir: &impl AsFd, name: &CStr, flags: AtFlags, btime: bool) -> Result<Stat, Errno> {
    let mut want = StatxFlags::TYPE
        | StatxFlags::NLINK
        | StatxFlags::UID
        | StatxFlags::INO
        | StatxFlags::BLOCKS
        | StatxFlags::MTIME;
    if btime {
        want |= StatxFlags::BTIME;
    }
    match retry(|| rustix::fs::statx(dir, name, flags, want)) {
        Ok(st) => Ok(Stat {
            kind: kind(FileType::from_raw_mode(u32::from(st.stx_mode))),
            dev: rustix::fs::makedev(st.stx_dev_major, st.stx_dev_minor),
            ino: st.stx_ino,
            nlink: st.stx_nlink,
            uid: st.stx_uid,
            blocks: st.stx_blocks,
            mtime: st.stx_mtime.tv_sec,
            btime: (btime && StatxFlags::from_bits_retain(st.stx_mask).contains(StatxFlags::BTIME))
                .then_some(st.stx_btime.tv_sec),
        }),
        Err(Errno::NOSYS | Errno::PERM) => {
            let st = retry(|| rustix::fs::statat(dir, name, flags & !AtFlags::STATX_DONT_SYNC))?;
            Ok(Stat {
                kind: kind(FileType::from_raw_mode(st.st_mode)),
                dev: st.st_dev,
                ino: st.st_ino,
                nlink: u32::try_from(st.st_nlink).unwrap_or(u32::MAX),
                uid: st.st_uid,
                blocks: u64::try_from(st.st_blocks).unwrap_or(0),
                mtime: st.st_mtime,
                btime: None,
            })
        }
        Err(e) => Err(e),
    }
}

fn kind(ft: FileType) -> EntryKind {
    match ft {
        FileType::Directory => EntryKind::Dir,
        FileType::RegularFile => EntryKind::File,
        FileType::Symlink => EntryKind::Symlink,
        _ => EntryKind::Other,
    }
}

/// Retried on `EINTR`, as std does: a signal is not an unreadable directory.
fn retry<T>(mut call: impl FnMut() -> Result<T, Errno>) -> Result<T, Errno> {
    loop {
        match call() {
            Err(Errno::INTR) => continue,
            done => return done,
        }
    }
}

fn read_error(e: Errno) -> ReadError {
    ReadError::from_errno(e.raw_os_error())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::platform::{meta, testing};
    use crate::scan::reader::{DirReader as _, StdReader};

    /// Name, kind, dev, ino, links, owner, allocated bytes, modified.
    type Row = (PathBuf, EntryKind, u64, u64, u32, Option<u32>, u64, Option<i64>);

    fn sorted(mut l: DirListing) -> (u64, u64, u64, Vec<Row>) {
        l.entries.sort_by(|a, b| a.name.cmp(&b.name));
        let rows = l
            .entries
            .into_iter()
            .map(|e| (PathBuf::from(e.name), e.kind, e.dev, e.ino, e.nlink, e.uid, e.alloc, e.modified))
            .collect();
        (l.dev, l.ino, l.alloc, rows)
    }

    /// The same tree, read by this reader and by `std`'s, lists the same
    /// entries: a symlink as a symlink, a hard link with its count, a
    /// non-UTF-8 name, a directory with its own blocks.
    #[test]
    fn it_lists_what_the_std_reader_lists() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("file"), vec![7u8; 10_000]).unwrap();
        fs::hard_link(d.path().join("file"), d.path().join("link")).unwrap();
        fs::create_dir(d.path().join("dir")).unwrap();
        testing::symlink(Path::new("/"), &d.path().join("sym")).unwrap();
        fs::write(d.path().join(testing::name_from_bytes(b"bad-\xff").unwrap()), b"x").unwrap();

        for times in [Times::None, Times::Modified] {
            let ours = sorted(list(d.path(), None, false, times).unwrap());
            let std = sorted(StdReader.read(d.path(), None, false, times).unwrap());
            assert_eq!(ours, std, "times: {times:?}");
        }
        let ours = sorted(list(d.path(), None, false, Times::None).unwrap()).3;
        assert_eq!(ours.len(), 5);
        let file = ours.iter().find(|r| r.0 == Path::new("file")).unwrap();
        assert_eq!((file.1, file.4, file.5), (EntryKind::File, 2, Some(crate::platform::procs::uid())));
        assert!(file.6 >= 8192, "allocated: {}", file.6);
        assert_eq!(ours.iter().find(|r| r.0 == Path::new("sym")).unwrap().1, EntryKind::Symlink);
        let dir_blocks = meta::alloc_bytes(&fs::symlink_metadata(d.path().join("dir")).unwrap());
        assert_eq!(ours.iter().find(|r| r.0 == Path::new("dir")).unwrap().6, dir_blocks, "a folder's own blocks");
    }

    /// The birth time stands in for the date added, and only when asked
    /// for: within a second of the file's writing, where the filesystem
    /// keeps one.
    #[test]
    fn the_date_added_is_the_birth_time_when_asked_for() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("f"), b"x").unwrap();
        let born = fs::metadata(d.path().join("f")).and_then(|m| m.created());
        for times in [Times::None, Times::Modified] {
            assert!(list(d.path(), None, false, times).unwrap().entries.iter().all(|e| e.added.is_none()));
        }
        let added = list(d.path(), None, false, Times::ModifiedAndAdded).unwrap().entries[0].added;
        match born {
            Ok(t) => {
                let secs = t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
                assert_eq!(added, Some(secs));
            }
            Err(_) => assert_eq!(added, None, "no birth time here"),
        }
    }

    /// The listing's own identity is the directory's, as `std` reads it.
    #[test]
    fn the_listing_carries_the_directorys_identity() {
        let d = tempfile::tempdir().unwrap();
        let id = meta::file_id(&fs::symlink_metadata(d.path()).unwrap()).unwrap();
        let l = list(d.path(), Some(id), false, Times::None).unwrap();
        assert_eq!((l.dev, l.ino), id);
        assert!(l.entries.is_empty());
    }

    /// A directory replaced since its parent was listed, or swapped for a
    /// symlink, is never read.
    #[test]
    fn a_swapped_directory_or_a_symlink_is_changed_and_never_read() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a");
        fs::create_dir(&a).unwrap();
        fs::write(a.join("inside"), b"x").unwrap();
        let id = meta::file_id(&fs::symlink_metadata(&a).unwrap()).unwrap();
        fs::rename(&a, d.path().join("old")).unwrap();
        fs::create_dir(&a).unwrap();
        assert_eq!(list(&a, Some(id), false, Times::None).unwrap_err().kind, ReadErrorKind::Changed);
        assert_eq!(
            list(&a, Some((id.0 ^ 1, id.1)), false, Times::None).unwrap_err().kind,
            ReadErrorKind::OtherFilesystem
        );

        fs::rename(&a, d.path().join("newer")).unwrap();
        testing::symlink(&d.path().join("old"), &a).unwrap();
        assert_eq!(list(&a, None, false, Times::None).unwrap_err().kind, ReadErrorKind::Changed);
    }

    #[test]
    fn a_missing_directory_is_not_found() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(list(&d.path().join("gone"), None, false, Times::None).unwrap_err().kind, ReadErrorKind::NotFound);
    }

    /// Unreadable to its owner: denied. Root reads it anyway, so the check
    /// holds only for an ordinary user, which the native job is.
    #[test]
    fn an_unreadable_directory_is_permission_denied() {
        use std::os::unix::fs::PermissionsExt as _;
        if rustix::process::geteuid().is_root() {
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let shut = d.path().join("shut");
        fs::create_dir(&shut).unwrap();
        fs::set_permissions(&shut, fs::Permissions::from_mode(0o000)).unwrap();
        let err = list(&shut, None, false, Times::None).unwrap_err();
        fs::set_permissions(&shut, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(err.kind, ReadErrorKind::PermissionDenied);
    }
}
