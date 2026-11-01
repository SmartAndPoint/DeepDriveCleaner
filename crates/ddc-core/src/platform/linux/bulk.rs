//! Directory reader on `getdents64(2)` and `statx(2)`: the directory is
//! opened without following a symlink, read in batches of names, and each
//! entry is stat'ed relative to the open directory, never following a
//! symlink. Linux has no bulk stat, so it is one `statx` per entry. No
//! private size: Linux keeps none. Linux keeps no date added either; the
//! birth time stands in for it where the filesystem records one (ext4,
//! btrfs, XFS), so a download's date is when it was written.
//!
//! The walk stays on one mount (safety ADR, amendment of 2026-11-01): each
//! entry is stat'ed without triggering an automount and marked as a
//! [`Boundary`] when it is an automount point or another mount, a bind
//! mount of the same filesystem included; and [`list_in`] opens a folder
//! beneath its scan root's descriptor with `openat2`, which refuses to
//! cross a mount or follow a symlink on the way.

use std::collections::HashMap;
use std::ffi::{CStr, OsStr};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, FileType, Mode, OFlags, RawDir, ResolveFlags, StatxAttributes, StatxFlags};
use rustix::io::Errno;

use crate::platform::types::{Boundary, DirListing, EntryKind, EntryMeta, ReadError, ReadErrorKind, Times};

const BUF_LEN: usize = 64 * 1024;

/// Linux's `PATH_MAX`, the terminating NUL included.
const PATH_MAX: usize = 4096;

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
    /// `stx_mnt_id`, where the kernel gives it (5.8 and later).
    mnt_id: Option<u64>,
    /// `STATX_ATTR_AUTOMOUNT` and `STATX_ATTR_MOUNT_ROOT`, each `None` where
    /// the kernel does not tell (`stx_attributes_mask`, or `fstatat`).
    automount: Option<bool>,
    mount_root: Option<bool>,
}

/// A scan root's open descriptor, which [`list_in`] opens folders beneath.
#[derive(Debug)]
pub struct Anchor {
    path: PathBuf,
    fd: OwnedFd,
    /// `openat2` works here; without it (`ENOSYS`, or `EPERM` from a
    /// seccomp filter) a folder opens by path and its mount is checked.
    beneath: bool,
    mnt_id: Option<u64>,
    /// mountinfo, read at most once for the whole walk under this root.
    mounts: std::sync::Mutex<Mounts>,
}

/// Opens `root` (canonical) as the anchor of a walk.
pub fn anchor(root: &Path) -> std::io::Result<Anchor> {
    let fd = retry(|| {
        rustix::fs::open(root, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())
    })?;
    // `ENOSYS`, `EPERM` from a seccomp filter, or anything else a sandbox
    // gives: open by path and check the mount instead.
    let beneath = openat2(&fd, c".").is_ok();
    let mnt_id = stat(&fd, c"", AtFlags::EMPTY_PATH, false)?.mnt_id;
    Ok(Anchor { path: root.to_path_buf(), fd, beneath, mnt_id, mounts: std::sync::Mutex::default() })
}

/// [`anchor`] as a kernel without `openat2` gives it: each folder opens by
/// path and its mount id is checked. The walk's fallback, which a test on a
/// real mount checks too.
pub fn anchor_by_path(root: &Path) -> std::io::Result<Anchor> {
    Ok(Anchor { beneath: false, ..anchor(root)? })
}

/// [`list`] of `dir`, a folder at or beneath `anchor`'s root, opened
/// relative to the root's descriptor: a mount or a symlink on the way is
/// refused (`EXDEV`, `ELOOP`), never crossed.
pub fn list_in(
    anchor: &Anchor,
    dir: &Path,
    expect: Option<(u64, u64)>,
    want_private: bool,
    times: Times,
) -> Result<DirListing, ReadError> {
    let Ok(rel) = dir.strip_prefix(&anchor.path) else {
        return Err(ReadError::new(ReadErrorKind::Changed, None));
    };
    // A folder whose whole path is past `PATH_MAX` stays unreadable, as it
    // is by path: its finding is incomplete, which the guard refuses, since
    // the clean removes by path (story "Linux file kinds: special files,
    // immutable flags, sparse files and odd names" takes such paths on).
    if dir.as_os_str().len() >= PATH_MAX {
        return Err(read_error(Errno::NAMETOOLONG));
    }
    let rel = rustix::ffi::CString::new(if rel.as_os_str().is_empty() {
        b".".to_vec()
    } else {
        rel.as_os_str().as_bytes().to_vec()
    })
    .map_err(|_| ReadError::new(ReadErrorKind::Other, None))?;
    let fd = if anchor.beneath {
        // `EAGAIN`: a rename raced the lookup; the kernel asks for a retry.
        let mut tries = 0;
        loop {
            match openat2(&anchor.fd, &rel) {
                Err(Errno::AGAIN) if tries < 3 => tries += 1,
                done => break done.map_err(read_error)?,
            }
        }
    } else {
        let fd = retry(|| {
            rustix::fs::open(
                dir,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
        })
        .map_err(read_error)?;
        let me = stat(&fd, c"", AtFlags::EMPTY_PATH, false).map_err(read_error)?;
        let crossed = (anchor.mnt_id.is_some() && me.mnt_id != anchor.mnt_id)
            || (dir != anchor.path && me.mount_root == Some(true));
        if crossed {
            return Err(ReadError::new(ReadErrorKind::OtherFilesystem, None));
        }
        fd
    };
    list_fd(&fd, dir, expect, want_private, times, &anchor.mounts)
}

fn openat2(dir: &OwnedFd, rel: &CStr) -> Result<OwnedFd, Errno> {
    retry(|| {
        rustix::fs::openat2(
            dir,
            rel,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_XDEV | ResolveFlags::NO_SYMLINKS,
        )
    })
}

pub fn list(dir: &Path, expect: Option<(u64, u64)>, want_private: bool, times: Times) -> Result<DirListing, ReadError> {
    let fd = retry(|| {
        rustix::fs::open(dir, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())
    })
    .map_err(read_error)?;
    list_fd(&fd, dir, expect, want_private, times, &std::sync::Mutex::default())
}

fn list_fd(
    fd: &OwnedFd,
    dir: &Path,
    expect: Option<(u64, u64)>,
    _want_private: bool,
    times: Times,
    mounts: &std::sync::Mutex<Mounts>,
) -> Result<DirListing, ReadError> {
    let me = stat(fd, c"", AtFlags::EMPTY_PATH, false).map_err(read_error)?;
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
    let mut names = RawDir::new(fd, &mut buf);
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
        let (mut entry, seen) = entry_meta(fd, name, times, me.mnt_id);
        if entry.kind == EntryKind::Dir {
            let mut mounts = mounts.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            entry.boundary = mounts.boundary(entry.boundary, seen, dir, &entry.name);
        }
        entries.push(entry);
    }
    // Counted once the listing is whole; a listing that fails part-way counts nothing.
    crate::stats::add(crate::stats::Counter::DirEntriesRead, entries.len() as u64);
    Ok(DirListing { dev: me.dev, ino: me.ino, modified: Some(me.mtime), alloc: me.blocks * 512, entries })
}

/// One entry, stat'ed relative to `dir` without following a symlink or
/// triggering an automount. An entry that cannot be stat'ed (gone since the
/// listing, or refused) carries its errno, as the reference reader's does.
/// `dir_mnt` is the folder's own mount id.
fn entry_meta(dir: &OwnedFd, name: &CStr, times: Times, dir_mnt: Option<u64>) -> (EntryMeta, Seen) {
    let name_os = OsStr::from_bytes(name.to_bytes()).to_owned();
    let added = times == Times::ModifiedAndAdded;
    let flags = AtFlags::SYMLINK_NOFOLLOW | AtFlags::STATX_DONT_SYNC | AtFlags::NO_AUTOMOUNT;
    match stat(dir, name, flags, added) {
        Ok(st) => {
            let seen = Seen { mnt_id: st.mnt_id, told: st.mount_root.is_some() };
            let meta = EntryMeta {
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
                boundary: boundary(st.automount, st.mount_root, st.mnt_id, dir_mnt),
            };
            (meta, seen)
        }
        Err(e) => {
            let meta = EntryMeta {
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
                boundary: None,
            };
            (meta, Seen { mnt_id: None, told: false })
        }
    }
}

/// What the entry's own stat told of its mount.
#[derive(Clone, Copy)]
struct Seen {
    mnt_id: Option<u64>,
    /// The kernel said whether it is a mount root (5.8 and later, `statx`).
    told: bool,
}

/// The edge of the walk an entry is, from its stat alone: an automount
/// point the kernel marks (NFS and SMB referrals, AFS, FUSE submounts), a
/// mount root, or a mount other than its folder's. An autofs mount root is
/// told apart from another mount only by mountinfo ([`Mounts`]).
fn boundary(
    automount: Option<bool>,
    mount_root: Option<bool>,
    mnt_id: Option<u64>,
    dir_mnt: Option<u64>,
) -> Option<Boundary> {
    if automount == Some(true) {
        Some(Boundary::Automount)
    } else if mount_root == Some(true) || matches!((mnt_id, dir_mnt), (Some(a), Some(b)) if a != b) {
        Some(Boundary::Mount)
    } else {
        None
    }
}

/// `/proc/self/mountinfo`, read only when a listing meets a mount root (to
/// tell autofs) or a kernel that tells no mount root (below 5.8, or
/// `fstatat`): once per walk under an [`Anchor`], and again only for a mount
/// id it has not seen, one made during the walk.
#[derive(Debug, Default)]
struct Mounts {
    by_id: Option<HashMap<u64, String>>,
    points: Option<HashMap<PathBuf, String>>,
}

impl Mounts {
    fn boundary(&mut self, found: Option<Boundary>, seen: Seen, dir: &Path, name: &OsStr) -> Option<Boundary> {
        let autofs = |t: &str| if t == "autofs" { Boundary::Automount } else { Boundary::Mount };
        match (found, seen) {
            (Some(Boundary::Mount), Seen { mnt_id: Some(id), .. }) => {
                if self.by_id.as_ref().is_none_or(|m| !m.contains_key(&id)) {
                    self.by_id = Some(super::statfs::types_by_id().unwrap_or_default());
                }
                Some(self.by_id.as_ref().and_then(|m| m.get(&id)).map_or(Boundary::Mount, |t| autofs(t)))
            }
            (None, Seen { told: false, .. }) => {
                let points = self.points.get_or_insert_with(|| super::statfs::points().unwrap_or_default());
                points.get(&dir.join(name)).map(|t| autofs(t))
            }
            (found, _) => found,
        }
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
        | StatxFlags::MTIME
        | StatxFlags::MNT_ID;
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
            mnt_id: StatxFlags::from_bits_retain(st.stx_mask).contains(StatxFlags::MNT_ID).then_some(st.stx_mnt_id),
            // Kernels before 5.13 set the bit without adding it to the
            // mask; a set bit is never wrong.
            automount: if st.stx_attributes.contains(StatxAttributes::AUTOMOUNT) {
                Some(true)
            } else {
                attribute(&st, StatxAttributes::AUTOMOUNT)
            },
            mount_root: attribute(&st, StatxAttributes::MOUNT_ROOT),
        }),
        Err(Errno::NOSYS | Errno::PERM) => {
            let st = retry(|| rustix::fs::statat(dir, name, flags & !AtFlags::STATX_DONT_SYNC))?;
            Ok(Stat {
                kind: kind(FileType::from_raw_mode(st.st_mode)),
                dev: st.st_dev,
                ino: st.st_ino,
                // `nlink_t` is 64 bits on x86_64 and 32 on aarch64.
                #[allow(clippy::useless_conversion)]
                nlink: u32::try_from(st.st_nlink).unwrap_or(u32::MAX),
                uid: st.st_uid,
                blocks: u64::try_from(st.st_blocks).unwrap_or(0),
                mtime: st.st_mtime,
                btime: None,
                mnt_id: None,
                automount: None,
                mount_root: None,
            })
        }
        Err(e) => Err(e),
    }
}

/// One `stx_attributes` bit, or `None` when the kernel does not report it.
fn attribute(st: &rustix::fs::Statx, bit: StatxAttributes) -> Option<bool> {
    st.stx_attributes_mask.contains(bit).then(|| st.stx_attributes.contains(bit))
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

    /// Safety ADR, amendment of 2026-11-01: what one entry's stat makes it.
    #[test]
    fn an_automount_point_or_another_mount_is_the_edge_of_the_walk() {
        assert_eq!(boundary(Some(true), Some(false), Some(30), Some(30)), Some(Boundary::Automount));
        assert_eq!(boundary(Some(false), Some(true), Some(30), Some(30)), Some(Boundary::Mount), "a mount root");
        assert_eq!(boundary(Some(false), Some(false), Some(31), Some(30)), Some(Boundary::Mount), "another mount");
        assert_eq!(boundary(Some(false), Some(false), Some(30), Some(30)), None, "a folder of this mount");
        assert_eq!(boundary(None, None, None, None), None, "a kernel that tells nothing");
        assert_eq!(boundary(None, None, Some(31), None), None, "no folder mount to compare with");
        assert_eq!(boundary(Some(true), None, None, None), Some(Boundary::Automount), "the bit alone, before 5.13");
    }

    /// autofs marks its dentries, not the inode, so its mount root is told
    /// by mountinfo; and where the kernel tells no mount root (below 5.8,
    /// or `fstatat`), the mount points of mountinfo do.
    #[test]
    fn mountinfo_tells_an_autofs_root_and_a_mount_the_kernel_did_not_mark() {
        let mut m = Mounts {
            by_id: Some(HashMap::from([(40, "autofs".to_owned()), (41, "nfs4".to_owned())])),
            points: Some(HashMap::from([
                (PathBuf::from("/r/auto"), "autofs".to_owned()),
                (PathBuf::from("/r/bind"), "ext4".to_owned()),
            ])),
        };
        let marked = |id| Seen { mnt_id: Some(id), told: true };
        assert_eq!(
            m.boundary(Some(Boundary::Mount), marked(40), Path::new("/r"), OsStr::new("x")),
            Some(Boundary::Automount)
        );
        assert_eq!(
            m.boundary(Some(Boundary::Mount), marked(41), Path::new("/r"), OsStr::new("x")),
            Some(Boundary::Mount)
        );
        assert_eq!(
            m.boundary(None, marked(30), Path::new("/r"), OsStr::new("bind")),
            None,
            "the kernel said: no mount"
        );
        let untold = Seen { mnt_id: None, told: false };
        assert_eq!(m.boundary(None, untold, Path::new("/r"), OsStr::new("auto")), Some(Boundary::Automount));
        assert_eq!(m.boundary(None, untold, Path::new("/r"), OsStr::new("bind")), Some(Boundary::Mount));
        assert_eq!(m.boundary(None, untold, Path::new("/r"), OsStr::new("plain")), None);
    }

    /// A folder opens beneath its root, with `openat2` and without: the same
    /// listing; a symlink on the way, or a path outside the root, is
    /// `changed`, never followed.
    #[test]
    fn a_folder_opens_beneath_its_root_and_never_through_a_symlink() {
        let d = tempfile::tempdir().unwrap();
        let root = crate::platform::Canonical::canonical(d.path()).unwrap();
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a/b/f"), b"x").unwrap();
        testing::symlink(&root.join("a"), &root.join("via")).unwrap();
        let outside = tempfile::tempdir().unwrap();

        let opened = anchor(&root).unwrap();
        let by_path = anchor_by_path(&root).unwrap();
        for a in [&opened, &by_path] {
            let names = |dir: &Path| {
                let mut n: Vec<_> =
                    list_in(a, dir, None, false, Times::None).unwrap().entries.into_iter().map(|e| e.name).collect();
                n.sort();
                n
            };
            assert_eq!(names(&root.join("a/b")), vec![OsStr::new("f").to_owned()], "beneath: {}", a.beneath);
            assert_eq!(names(&root).len(), 2);
            let err = |dir: &Path| list_in(a, dir, None, false, Times::None).unwrap_err().kind;
            assert_eq!(err(outside.path()), ReadErrorKind::Changed, "outside the root");
            if a.beneath {
                assert_eq!(err(&root.join("via/b")), ReadErrorKind::Changed, "a symlink on the way");
            }
        }
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
