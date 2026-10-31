//! Directory reader on `getdents64(2)` and `statx(2)`: the directory is
//! opened without following a symlink, read in batches of names, and each
//! entry is stat'ed relative to the open directory, never following a
//! symlink. Linux has no bulk stat, so it is one `statx` per entry. No
//! private size and no date added: Linux keeps neither.

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
    blocks: u64,
    mtime: i64,
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
    let me = stat(&fd, c"", AtFlags::EMPTY_PATH).map_err(read_error)?;
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
    Ok(DirListing { dev: me.dev, ino: me.ino, modified: Some(me.mtime), entries })
}

/// One entry, stat'ed relative to `dir` without following a symlink. An
/// entry that cannot be stat'ed (gone since the listing, or refused) carries
/// its errno, as the reference reader's does.
fn entry_meta(dir: &OwnedFd, name: &CStr, times: Times) -> EntryMeta {
    let name_os = OsStr::from_bytes(name.to_bytes()).to_owned();
    match stat(dir, name, AtFlags::SYMLINK_NOFOLLOW | AtFlags::STATX_DONT_SYNC) {
        Ok(st) => EntryMeta {
            name: name_os,
            kind: st.kind,
            dev: st.dev,
            ino: st.ino,
            nlink: st.nlink,
            flags: 0,
            alloc: if st.kind == EntryKind::Dir { 0 } else { st.blocks * 512 },
            private: None,
            error: None,
            modified: (times != Times::None).then_some(st.mtime),
            added: None,
        },
        Err(e) => EntryMeta {
            name: name_os,
            kind: EntryKind::Other,
            dev: 0,
            ino: 0,
            nlink: 1,
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
/// numbers, which is what std's `MetadataExt::dev` gives.
fn stat(dir: &impl AsFd, name: &CStr, flags: AtFlags) -> Result<Stat, Errno> {
    let want = StatxFlags::TYPE | StatxFlags::NLINK | StatxFlags::INO | StatxFlags::BLOCKS | StatxFlags::MTIME;
    match retry(|| rustix::fs::statx(dir, name, flags, want)) {
        Ok(st) => Ok(Stat {
            kind: kind(FileType::from_raw_mode(u32::from(st.stx_mode))),
            dev: rustix::fs::makedev(st.stx_dev_major, st.stx_dev_minor),
            ino: st.stx_ino,
            nlink: st.stx_nlink,
            blocks: st.stx_blocks,
            mtime: st.stx_mtime.tv_sec,
        }),
        Err(Errno::NOSYS | Errno::PERM) => {
            let st = retry(|| rustix::fs::statat(dir, name, flags & !AtFlags::STATX_DONT_SYNC))?;
            Ok(Stat {
                kind: kind(FileType::from_raw_mode(st.st_mode)),
                dev: st.st_dev,
                ino: st.st_ino,
                nlink: u32::try_from(st.st_nlink).unwrap_or(u32::MAX),
                blocks: u64::try_from(st.st_blocks).unwrap_or(0),
                mtime: st.st_mtime,
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

    /// Name, kind, dev, ino, links, allocated bytes, modified.
    type Row = (PathBuf, EntryKind, u64, u64, u32, u64, Option<i64>);

    fn sorted(mut l: DirListing) -> (u64, u64, Vec<Row>) {
        l.entries.sort_by(|a, b| a.name.cmp(&b.name));
        let rows = l
            .entries
            .into_iter()
            .map(|e| (PathBuf::from(e.name), e.kind, e.dev, e.ino, e.nlink, e.alloc, e.modified))
            .collect();
        (l.dev, l.ino, rows)
    }

    /// The same tree, read by this reader and by `std`'s, lists the same
    /// entries: a symlink as a symlink, a hard link with its count, a
    /// non-UTF-8 name, a directory with no allocation of its own.
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
        let ours = sorted(list(d.path(), None, false, Times::None).unwrap()).2;
        assert_eq!(ours.len(), 5);
        let file = ours.iter().find(|r| r.0 == Path::new("file")).unwrap();
        assert_eq!((file.1, file.4), (EntryKind::File, 2));
        assert!(file.5 >= 8192, "allocated: {}", file.5);
        assert_eq!(ours.iter().find(|r| r.0 == Path::new("sym")).unwrap().1, EntryKind::Symlink);
        assert_eq!(ours.iter().find(|r| r.0 == Path::new("dir")).unwrap().5, 0);
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
