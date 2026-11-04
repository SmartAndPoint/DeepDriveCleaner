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
//! cross a mount or follow a symlink on the way. The guard opens a parent
//! the same way (`open_in`), stricter: without a mount id to compare, it
//! refuses.

use std::collections::HashMap;
use std::ffi::{CStr, OsStr};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd as _, OwnedFd};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use rustix::fs::{AtFlags, FileType, Mode, OFlags, RawDir, ResolveFlags, StatxAttributes, StatxFlags};
use rustix::io::Errno;

use super::fsid::ZFS_SUPER_MAGIC;
use super::statfs::Mount;
use crate::platform::types::{Boundary, DirListing, EntryKind, EntryMeta, EntryStat, ReadError, ReadErrorKind, Times};

const BUF_LEN: usize = 64 * 1024;

/// Linux's `PATH_MAX`, the terminating NUL included.
const PATH_MAX: usize = 4096;

/// `statfs`'s `f_type` of an open folder's filesystem, as the 32-bit magic
/// number it is.
pub(super) fn fs_magic(fd: &OwnedFd) -> Option<u32> {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a magic number fits 32 bits
    rustix::fs::fstatfs(fd).ok().map(|st| st.f_type as u32)
}

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
    /// `stx_mnt_id`, where the kernel gives it (5.8 and later). An open
    /// descriptor's is [`mount_id`].
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
    let mnt_id = mount_id(&fd, stat(&fd, c"", AtFlags::EMPTY_PATH, false)?.mnt_id);
    Ok(Anchor { path: root.to_path_buf(), fd, beneath, mnt_id, mounts: std::sync::Mutex::default() })
}

/// [`anchor`] as a kernel without `openat2` gives it: each folder opens by
/// path and its mount id is checked. The walk's fallback, which a test on a
/// real mount checks too.
pub fn anchor_by_path(root: &Path) -> std::io::Result<Anchor> {
    Ok(Anchor { beneath: false, ..anchor(root)? })
}

/// The anchor of the ZFS child dataset mounted at `dir`, a folder beneath
/// `parent`'s root, for the walk inside it (safety ADR, amendment of
/// 2026-11-02, "btrfs and ZFS on Linux"): its folder above opened beneath
/// `parent`, the dataset's root opened from there, and refused unless it
/// is the root of `vouched`, the mount mountinfo vouched for. Every folder
/// inside opens beneath it, crossing no further mount.
pub fn anchor_beneath(parent: &Anchor, dir: &Path, vouched: u64) -> std::io::Result<Anchor> {
    dataset_anchor(parent, dir, vouched, false)
}

/// The anchor of the innermost ZFS child dataset `dir` lies in below
/// `root`'s folder, each one anchored beneath the one above, as the walk
/// anchors them: where the guard opens an item's parent. `None` when `dir`
/// lies in none; an error when mountinfo cannot be read or a dataset on the
/// way is not the one it lists.
pub fn anchor_for(root: &Anchor, dir: &Path) -> std::io::Result<Option<Anchor>> {
    // Only a ZFS mount has child datasets: any other root reads no mountinfo.
    let Some(mut id) = root.mnt_id else { return Ok(None) };
    if fs_magic(&root.fd) != Some(ZFS_SUPER_MAGIC) {
        return Ok(None);
    }
    let vouches = |lines: &[Mount], parent: u64, child: u64, at: &Path| {
        let line = |id: u64| lines.iter().find(|m| m.id == id);
        line(child).zip(line(parent)).is_some_and(|(c, p)| super::statfs::zfs_child(c, p) && c.mount_point == at)
    };
    let mut lines = super::statfs::lines()?;
    let mut inner: Option<Anchor> = None;
    loop {
        let here = inner.as_ref().unwrap_or(root);
        let Some(parent) = lines.iter().find(|m| m.id == id) else { break };
        let Some(next) = lines.iter().find(|c| {
            super::statfs::zfs_child(c, parent)
                && c.mount_point != here.path
                && c.mount_point.starts_with(&here.path)
                && dir.starts_with(&c.mount_point)
        }) else {
            break;
        };
        let (next, at) = (next.id, next.mount_point.clone());
        let held = dataset_anchor(here, &at, next, true)?;
        // The descriptor pins mount `next`: mountinfo read again must still
        // vouch for it, so a dataset swapped between the read and the open
        // is refused.
        lines = super::statfs::lines()?;
        if !vouches(&lines, id, next, &at) {
            return Err(std::io::Error::other(format!("{} changed since mountinfo was read", at.display())));
        }
        id = next;
        inner = Some(held);
    }
    Ok(inner)
}

/// [`anchor_beneath`]; `strict`, the guard's ([`open_in`]).
fn dataset_anchor(parent: &Anchor, dir: &Path, vouched: u64, strict: bool) -> std::io::Result<Anchor> {
    let (Some(up), Some(name)) = (dir.parent(), dir.file_name()) else {
        return Err(std::io::ErrorKind::InvalidInput.into());
    };
    let up = open_in(parent, up, strict).map_err(|e| {
        e.errno.map_or_else(|| std::io::Error::other(format!("{:?}", e.kind)), std::io::Error::from_raw_os_error)
    })?;
    let name = rustix::ffi::CString::new(name.as_bytes()).map_err(|_| std::io::ErrorKind::InvalidInput)?;
    let fd = retry(|| {
        rustix::fs::openat(
            &up,
            &*name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
    })?;
    let st = stat(&fd, c"", AtFlags::EMPTY_PATH, false)?;
    if mount_id(&fd, st.mnt_id) != Some(vouched) || is_mount_root(&fd) != Some(true) {
        return Err(std::io::Error::other(format!("{} is not the root of mount {vouched}", dir.display())));
    }
    Ok(Anchor {
        path: dir.to_path_buf(),
        fd,
        beneath: parent.beneath,
        mnt_id: Some(vouched),
        mounts: std::sync::Mutex::default(),
    })
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
    let fd = open_in(anchor, dir, false)?;
    list_fd(&fd, dir, expect, want_private, times, &anchor.mounts)
}

/// `dir`, a folder at or beneath `anchor`'s root, opened relative to the
/// root's descriptor (`openat2`), or by path with its mount id checked
/// against the root's; by path, only the last component is never a
/// symlink, as before. `strict`, the guard's: the root's path must still
/// name the anchor's folder, and a folder whose mount id or the root's is
/// unknown (no `STATX_MNT_ID`, no `/proc`) is refused as another
/// filesystem. The scan reads on there, with `dev` and mountinfo (safety
/// ADR, amendment of 2026-11-01, completed on 2026-11-02).
pub(super) fn open_in(anchor: &Anchor, dir: &Path, strict: bool) -> Result<OwnedFd, ReadError> {
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
    if strict {
        // The guard's anchor outlives the scan: a root renamed away or
        // mounted over since is not the folder its path names now.
        let now = retry(|| rustix::fs::lstat(&anchor.path)).map_err(read_error)?;
        let held = retry(|| rustix::fs::fstat(&anchor.fd)).map_err(read_error)?;
        if (now.st_dev, now.st_ino) != (held.st_dev, held.st_ino) {
            return Err(ReadError::new(ReadErrorKind::Changed, None));
        }
    }
    if anchor.beneath {
        // `EAGAIN`: a rename raced the lookup; the kernel asks for a retry.
        let mut tries = 0;
        loop {
            match openat2(&anchor.fd, &rel) {
                Err(Errno::AGAIN) if tries < 3 => tries += 1,
                done => return done.map_err(read_error),
            }
        }
    }
    let fd = retry(|| {
        rustix::fs::open(dir, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())
    })
    .map_err(read_error)?;
    let me = stat(&fd, c"", AtFlags::EMPTY_PATH, false).map_err(read_error)?;
    let crossed = match (anchor.mnt_id, mount_id(&fd, me.mnt_id)) {
        (Some(root), Some(id)) => id != root,
        _ => strict,
    } || (dir != anchor.path && me.mount_root == Some(true));
    if crossed {
        return Err(ReadError::new(ReadErrorKind::OtherFilesystem, None));
    }
    Ok(fd)
}

/// An open descriptor's mount id: `statx`'s, or where it gives none (below
/// kernel 5.8, or a seccomp filter that refuses `statx`), the `mnt_id:`
/// line of `/proc/self/fdinfo/<fd>` (3.15). The descriptor pins its mount,
/// so the answer cannot be about a mount that came and went. `None` when
/// neither tells.
fn mount_id(fd: &OwnedFd, statx: Option<u64>) -> Option<u64> {
    statx.or_else(|| {
        let info = std::fs::read(format!("/proc/self/fdinfo/{}", fd.as_raw_fd())).ok()?;
        fdinfo_mnt_id(&info)
    })
}

/// An open folder's `(dev, ino)`, its device the reported one
/// (`super::fsid`), and mount id ([`mount_id`]), for the Trash's choice of
/// a trash folder on the item's own mount.
pub(super) fn fd_mount(fd: &OwnedFd) -> Result<((u64, u64), Option<u64>), Errno> {
    let st = stat(fd, c"", AtFlags::EMPTY_PATH, false)?;
    let mnt = mount_id(fd, st.mnt_id);
    Ok(((super::fsid::of_fd(fd, st.dev, mnt)?, st.ino), mnt))
}

/// What an open file is, read once from its descriptor: [`held`].
pub(super) struct Held {
    /// `st_dev`, as the kernel gives it.
    pub(super) raw: u64,
    /// `(dev, ino)`, the device the reported one (`super::fsid`).
    pub(super) id: (u64, u64),
    pub(super) kind: EntryKind,
}

/// An open file (an `O_PATH` descriptor too): one `statx`, its mount id,
/// and the reported device.
pub(super) fn held(fd: &OwnedFd) -> Result<Held, Errno> {
    let st = stat(fd, c"", AtFlags::EMPTY_PATH, false)?;
    let dev = super::fsid::of_fd(fd, st.dev, mount_id(fd, st.mnt_id))?;
    Ok(Held { raw: st.dev, id: (dev, st.ino), kind: st.kind })
}

/// An open folder is the root of its mount: `STATX_ATTR_MOUNT_ROOT` where
/// the kernel tells, else its mount id differs from that of its `..`, or
/// its `..` is itself (the process's root). No path is compared (safety
/// ADR, amendment of 2026-11-01, completed on 2026-11-02). `None` when
/// neither tells.
pub(super) fn is_mount_root(fd: &OwnedFd) -> Option<bool> {
    let st = stat(fd, c"", AtFlags::EMPTY_PATH, false).ok()?;
    if let Some(root) = st.mount_root {
        return Some(root);
    }
    let up = retry(|| rustix::fs::openat(fd, c"..", OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty()))
        .ok()?;
    let above = stat(&up, c"", AtFlags::EMPTY_PATH, false).ok()?;
    if (above.dev, above.ino) == (st.dev, st.ino) {
        return Some(true);
    }
    Some(mount_id(fd, st.mnt_id)? != mount_id(&up, above.mnt_id)?)
}

fn fdinfo_mnt_id(info: &[u8]) -> Option<u64> {
    std::str::from_utf8(info).ok()?.lines().find_map(|l| l.strip_prefix("mnt_id:")?.trim().parse().ok())
}

/// One child of `dir` for the guard, stat'ed without following a symlink or
/// triggering an automount: a mount root, an automount point or another
/// mount than `dir`'s is `mount_root`. Without a mount id (no
/// `STATX_MNT_ID`) only the kernel's own mark tells, and a bind mount there
/// is caught by its `(dev, ino)` alone, as the amendment accepts. Its
/// device is the reported one ([`child_device`]).
pub(super) fn stat_child(dir: &OwnedFd, name: &OsStr) -> Result<EntryStat, Errno> {
    let me = stat(dir, c"", AtFlags::EMPTY_PATH, false)?;
    let dir_mnt = me.mnt_id;
    let st = stat(dir, name, AtFlags::SYMLINK_NOFOLLOW | AtFlags::NO_AUTOMOUNT, false)?;
    let folder = (me.dev, super::fsid::of_fd(dir, me.dev, mount_id(dir, dir_mnt))?);
    let name = rustix::ffi::CString::new(name.as_bytes()).map_err(|_| Errno::INVAL)?;
    let dev =
        child_device(dir, &name, (st.dev, st.ino), st.automount == Some(true), folder, &std::sync::Mutex::default())?;
    Ok(EntryStat {
        dev,
        ino: st.ino,
        is_dir: st.kind == EntryKind::Dir,
        is_file: st.kind == EntryKind::File,
        is_symlink: st.kind == EntryKind::Symlink,
        flags: 0,
        alloc: st.blocks * 512,
        mount_root: boundary(st.automount, st.mount_root, st.mnt_id, dir_mnt).is_some(),
    })
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
    // This folder's mount, for the subvolume rule: unknown, nothing is entered.
    let dir_mnt = mount_id(fd, me.mnt_id);
    let dev = super::fsid::of_fd(fd, me.dev, dir_mnt).map_err(read_error)?;
    if let Some((want_dev, want_ino)) = expect {
        if dev != want_dev {
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
            let mut cache = mounts.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            entry.boundary = cache.boundary(entry.boundary, seen, dir, &entry.name, dir_mnt);
            // `.zfs` at a dataset's root: its snapshots mount as they are
            // opened, and the kernel marks none of them as an automount
            // point. Told by its name, never entered (safety ADR, amendment
            // of 2026-11-02, "btrfs and ZFS on Linux").
            if name == c".zfs" && cache.zfs_root(dir_mnt, dir, me.mount_root, || fs_magic(fd)) {
                entry.boundary = Some(Boundary::Snapshot);
            }
            drop(cache);
            // Same mount, another device: a btrfs subvolume reached by path,
            // or something else the walk must not enter.
            if entry.boundary.is_none() && entry.error.is_none() && entry.dev != me.dev {
                entry.boundary = Some(subvolume_edge(fd, name, (entry.dev, entry.ino), dir_mnt, mounts));
            }
        }
        // The raw devices told the edges above; the reported one replaces it last.
        if entry.error.is_none() {
            let automount = entry.boundary == Some(Boundary::Automount);
            match child_device(fd, name, (entry.dev, entry.ino), automount, (me.dev, dev), mounts) {
                Ok(reported) => entry.dev = reported,
                Err(e) => entry.error = Some(e.raw_os_error()),
            }
        }
        entries.push(entry);
    }
    // Counted once the listing is whole; a listing that fails part-way counts nothing.
    crate::stats::add(crate::stats::Counter::DirEntriesRead, entries.len() as u64);
    Ok(DirListing { dev, ino: me.ino, modified: Some(me.mtime), alloc: me.blocks * 512, entries })
}

/// The device the entry `name` of the folder `dir` reports, `id` its raw
/// `(dev, ino)` and `folder` the folder's raw and reported devices: the
/// folder's where the raw devices agree (one subvolume, one dataset);
/// else its own, read through an `O_PATH` descriptor, which triggers no
/// automount, and refused (`ENOENT`) unless it is the entry stat'ed. Only a
/// btrfs or ZFS mount, or one mountinfo does not name, is asked `fstatfs`:
/// another one (NFS, SMB) may wait on its server, and keeps its raw device
/// as it reports it anyway. An automount point keeps its raw device: it is
/// never entered, nor opened.
fn child_device(
    dir: &OwnedFd,
    name: &CStr,
    id: (u64, u64),
    automount: bool,
    folder: (u64, u64),
    mounts: &std::sync::Mutex<Mounts>,
) -> Result<u64, Errno> {
    if id.0 == folder.0 {
        return Ok(folder.1);
    }
    if automount {
        return Ok(id.0);
    }
    let fd = retry(|| rustix::fs::openat(dir, name, OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty()))?;
    let st = stat(&fd, c"", AtFlags::EMPTY_PATH | AtFlags::STATX_DONT_SYNC, false)?;
    if (st.dev, st.ino) != id {
        return Err(Errno::NOENT);
    }
    let mnt = mount_id(&fd, st.mnt_id);
    let kind = mnt.and_then(|m| mounts.lock().unwrap_or_else(std::sync::PoisonError::into_inner).type_of(m));
    match kind.as_deref() {
        Some("btrfs" | "zfs") | None => super::fsid::of_fd(&fd, st.dev, mnt),
        Some(_) => Ok(st.dev),
    }
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

/// A folder on this folder's mount but another device (safety ADR,
/// amendment of 2026-11-02, "btrfs and ZFS on Linux"). Entered only when the
/// mount is btrfs, the child opened beneath this folder is still the listed
/// one on the same known mount, and the kernel calls it a plain subvolume;
/// a snapshot is never entered; anything else, an error included, is
/// another filesystem.
/// The walk-wide mountinfo cache is locked for the type alone, never across
/// the open, the stat or the ioctl.
fn subvolume_edge(
    dir: &OwnedFd,
    name: &CStr,
    id: (u64, u64),
    dir_mnt: Option<u64>,
    mounts: &std::sync::Mutex<Mounts>,
) -> Boundary {
    let Some(dir_mnt) = dir_mnt else { return Boundary::Mount };
    let kind = mounts.lock().unwrap_or_else(std::sync::PoisonError::into_inner).type_of(dir_mnt);
    if kind.as_deref() != Some("btrfs") {
        return Boundary::Mount;
    }
    let Ok(child) = retry(|| {
        rustix::fs::openat(
            dir,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
    }) else {
        return Boundary::Mount;
    };
    let Ok(st) = stat(&child, c"", AtFlags::EMPTY_PATH, false) else { return Boundary::Mount };
    if (st.dev, st.ino) != id || mount_id(&child, st.mnt_id) != Some(dir_mnt) {
        return Boundary::Mount;
    }
    match super::btrfs::subvolume(&child) {
        Ok(super::btrfs::Subvolume::Plain) => Boundary::Subvolume,
        Ok(super::btrfs::Subvolume::Snapshot) => Boundary::Snapshot,
        Err(_) => Boundary::Mount,
    }
}

/// Every step of [`subvolume_edge`] for `name` in `dir`, with its values,
/// for a CI test that can only see a real btrfs on the runner: the walk
/// never calls it.
#[doc(hidden)]
pub fn subvolume_trace(dir: &Path, name: &OsStr) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let fd = match retry(|| {
        rustix::fs::open(dir, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())
    }) {
        Ok(fd) => fd,
        Err(e) => return format!("open {}: {e}", dir.display()),
    };
    let fdinfo = |fd: &OwnedFd| {
        std::fs::read(format!("/proc/self/fdinfo/{}", fd.as_raw_fd())).ok().and_then(|i| fdinfo_mnt_id(&i))
    };
    let me = stat(&fd, c"", AtFlags::EMPTY_PATH, false);
    let me_mnt = me.as_ref().ok().and_then(|m| m.mnt_id);
    match &me {
        Ok(m) => {
            let _ = writeln!(
                out,
                "parent: dev {} ino {} statx mnt_id {:?} mount_root {:?} fdinfo mnt_id {:?}",
                m.dev,
                m.ino,
                m.mnt_id,
                m.mount_root,
                fdinfo(&fd)
            );
        }
        Err(e) => {
            let _ = writeln!(out, "parent stat: {e}");
        }
    }
    let dir_mnt = mount_id(&fd, me_mnt);
    let mut mounts = Mounts::default();
    let kind = dir_mnt.and_then(|id| mounts.type_of(id));
    let line = dir_mnt.and_then(|id| {
        let info = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
        info.lines().find(|l| l.split(' ').next() == Some(id.to_string().as_str())).map(str::to_owned)
    });
    let _ = writeln!(out, "parent mount: {dir_mnt:?} type {kind:?} line {line:?}");
    let Ok(cname) = rustix::ffi::CString::new(name.as_bytes()) else { return out + "name has a NUL" };
    let flags = AtFlags::SYMLINK_NOFOLLOW | AtFlags::STATX_DONT_SYNC | AtFlags::NO_AUTOMOUNT;
    match stat(&fd, &*cname, flags, false) {
        Ok(st) => {
            let _ = writeln!(
                out,
                "entry: dev {} ino {} statx mnt_id {:?} mount_root {:?} automount {:?} boundary {:?}",
                st.dev,
                st.ino,
                st.mnt_id,
                st.mount_root,
                st.automount,
                boundary(st.automount, st.mount_root, st.mnt_id, me_mnt)
            );
        }
        Err(e) => {
            let _ = writeln!(out, "entry stat: {e}");
        }
    }
    let (meta, seen) = entry_meta(&fd, &cname, Times::None, me_mnt);
    let edge = mounts.boundary(meta.boundary, seen, dir, &meta.name, dir_mnt);
    let _ = writeln!(out, "entry_meta: boundary {:?} then mounts.boundary {edge:?} told {}", meta.boundary, seen.told);
    match retry(|| {
        rustix::fs::openat(
            &fd,
            &*cname,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
    }) {
        Ok(child) => {
            match stat(&child, c"", AtFlags::EMPTY_PATH, false) {
                Ok(st) => {
                    let _ = writeln!(
                        out,
                        "child opened: dev {} ino {} statx mnt_id {:?} fdinfo mnt_id {:?} mount_id {:?}",
                        st.dev,
                        st.ino,
                        st.mnt_id,
                        fdinfo(&child),
                        mount_id(&child, st.mnt_id)
                    );
                }
                Err(e) => {
                    let _ = writeln!(out, "child stat: {e}");
                }
            }
            let _ = writeln!(out, "ioctl: {:?}", super::btrfs::subvolume(&child));
        }
        Err(e) => {
            let _ = writeln!(out, "child open: {e}");
        }
    }
    let shared = std::sync::Mutex::new(Mounts::default());
    let _ = write!(out, "edge: {:?}", subvolume_edge(&fd, &cname, (meta.dev, meta.ino), dir_mnt, &shared));
    out
}

/// The dataset rule for `name` in `dir`, condition by condition, with the
/// mountinfo lines it read and the anchor it gives, for a CI test that can
/// only see a real ZFS on the runner: the walk never calls it.
#[doc(hidden)]
pub fn dataset_trace(dir: &Path, name: &OsStr) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let held = match anchor(dir) {
        Ok(held) => held,
        Err(e) => return format!("anchor {}: {e}", dir.display()),
    };
    let _ = writeln!(out, "folder: mount {:?} beneath {}", held.mnt_id, held.beneath);
    let Ok(cname) = rustix::ffi::CString::new(name.as_bytes()) else { return out + "name has a NUL" };
    let (meta, seen) = entry_meta(&held.fd, &cname, Times::None, held.mnt_id);
    let _ = writeln!(
        out,
        "entry: dev {} ino {} mnt_id {:?} told {} boundary {:?}",
        meta.dev, meta.ino, seen.mnt_id, seen.told, meta.boundary
    );
    let mut mounts = Mounts::default();
    let child = seen.mnt_id.and_then(|id| mounts.line(id).cloned());
    let parent = held.mnt_id.and_then(|id| mounts.line(id).cloned());
    let _ = writeln!(out, "child line: {child:?}\nparent line: {parent:?}");
    if let (Some(c), Some(p)) = (&child, &parent) {
        for (what, holds) in super::statfs::zfs_conditions(c, p) {
            let _ = writeln!(out, "  {what}: {holds}");
        }
        let _ = writeln!(out, "  mounted at the entry: {}", c.mount_point == dir.join(name));
    }
    let edge = mounts.boundary(meta.boundary, seen, dir, &meta.name, held.mnt_id);
    let _ = writeln!(out, "edge: {edge:?}");
    let held_inside = match edge {
        Some(Boundary::Dataset(id)) => format!("{:?}", anchor_beneath(&held, &dir.join(name), id).map(|a| a.mnt_id)),
        _ => "none".to_owned(),
    };
    let _ = write!(out, "anchor: {held_inside}");
    out
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
    by_id: Option<HashMap<u64, Mount>>,
    points: Option<HashMap<PathBuf, String>>,
}

impl Mounts {
    /// mountinfo's line for mount `id`, read again for an id it has not
    /// seen.
    fn line(&mut self, id: u64) -> Option<&Mount> {
        if self.by_id.as_ref().is_none_or(|m| !m.contains_key(&id)) {
            let lines = super::statfs::lines().unwrap_or_default();
            self.by_id = Some(lines.into_iter().map(|m| (m.id, m)).collect());
        }
        self.by_id.as_ref()?.get(&id)
    }

    /// The type of mount `id`.
    fn type_of(&mut self, id: u64) -> Option<String> {
        self.line(id).map(|m| m.fs_type.clone())
    }

    /// The folder `dir`, on mount `dir_mnt`, is the root of a ZFS dataset
    /// ([`zfs_root`]).
    fn zfs_root(
        &mut self,
        dir_mnt: Option<u64>,
        dir: &Path,
        mount_root: Option<bool>,
        magic: impl FnOnce() -> Option<u32>,
    ) -> bool {
        zfs_root(dir_mnt.and_then(|id| self.line(id)), dir, mount_root, magic)
    }

    /// What mountinfo makes of an entry `name` of `dir`, a folder on mount
    /// `dir_mnt`: an autofs root is an automount point, a ZFS child dataset
    /// of the folder's mount, mounted at the entry, is [`Boundary::Dataset`]
    /// (safety ADR, amendment of 2026-11-02, "btrfs and ZFS on Linux"), any
    /// other mount root stays a mount.
    fn boundary(
        &mut self,
        found: Option<Boundary>,
        seen: Seen,
        dir: &Path,
        name: &OsStr,
        dir_mnt: Option<u64>,
    ) -> Option<Boundary> {
        let autofs = |t: &str| if t == "autofs" { Boundary::Automount } else { Boundary::Mount };
        match (found, seen) {
            (Some(Boundary::Mount), Seen { mnt_id: Some(id), .. }) => {
                let Some(child) = self.line(id).cloned() else { return Some(Boundary::Mount) };
                if child.fs_type != "zfs" {
                    return Some(autofs(&child.fs_type));
                }
                let parent = dir_mnt.and_then(|d| self.line(d));
                let dataset =
                    parent.is_some_and(|p| super::statfs::zfs_child(&child, p) && child.mount_point == dir.join(name));
                Some(if dataset { Boundary::Dataset(id) } else { Boundary::Mount })
            }
            (None, Seen { told: false, .. }) => {
                let points = self.points.get_or_insert_with(|| super::statfs::points().unwrap_or_default());
                points.get(&dir.join(name)).map(|t| autofs(t))
            }
            (found, _) => found,
        }
    }
}

/// The folder `dir` is the root of a ZFS dataset, where `.zfs` is the
/// snapshot folder: by mountinfo's `line` for its mount, ZFS and mounted
/// there or a mount root the kernel tells; without a line, by its own
/// filesystem's `magic` and a mount root the kernel does not deny. An
/// unknown answer, of either, is a dataset's root: `.zfs` is never entered.
fn zfs_root(line: Option<&Mount>, dir: &Path, mount_root: Option<bool>, magic: impl FnOnce() -> Option<u32>) -> bool {
    match line {
        Some(m) => m.fs_type == "zfs" && (mount_root == Some(true) || m.mount_point == dir),
        None => magic().is_none_or(|m| m == ZFS_SUPER_MAGIC) && mount_root != Some(false),
    }
}

/// `statx`, or `fstatat` where the kernel or a seccomp filter refuses
/// `statx` (`ENOSYS`, `EPERM`). `dev` is `makedev` of the major and minor
/// numbers, which is what std's `MetadataExt::dev` gives. The birth time
/// only when `btime` asks for it and `statx` returns it.
fn stat<P: rustix::path::Arg + Copy>(dir: &impl AsFd, name: P, flags: AtFlags, btime: bool) -> Result<Stat, Errno> {
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
pub(super) fn retry<T>(mut call: impl FnMut() -> Result<T, Errno>) -> Result<T, Errno> {
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
        let id = meta::lstat(d.path()).unwrap().id();
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
        let id = meta::lstat(&a).unwrap().id();
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
        let line = |id: u64, fs_type: &str| Mount {
            id,
            parent: 1,
            root: "/".into(),
            mount_point: format!("/m/{id}").into(),
            fs_type: fs_type.to_owned(),
            source: "none".to_owned(),
            super_options: String::new(),
        };
        let mut m = Mounts {
            by_id: Some(HashMap::from([(40, line(40, "autofs")), (41, line(41, "nfs4"))])),
            points: Some(HashMap::from([
                (PathBuf::from("/r/auto"), "autofs".to_owned()),
                (PathBuf::from("/r/bind"), "ext4".to_owned()),
            ])),
        };
        let marked = |id| Seen { mnt_id: Some(id), told: true };
        assert_eq!(
            m.boundary(Some(Boundary::Mount), marked(40), Path::new("/r"), OsStr::new("x"), Some(30)),
            Some(Boundary::Automount)
        );
        assert_eq!(
            m.boundary(Some(Boundary::Mount), marked(41), Path::new("/r"), OsStr::new("x"), Some(30)),
            Some(Boundary::Mount)
        );
        assert_eq!(
            m.boundary(None, marked(30), Path::new("/r"), OsStr::new("bind"), Some(30)),
            None,
            "the kernel said: no mount"
        );
        let untold = Seen { mnt_id: None, told: false };
        assert_eq!(m.boundary(None, untold, Path::new("/r"), OsStr::new("auto"), None), Some(Boundary::Automount));
        assert_eq!(m.boundary(None, untold, Path::new("/r"), OsStr::new("bind"), None), Some(Boundary::Mount));
        assert_eq!(m.boundary(None, untold, Path::new("/r"), OsStr::new("plain"), None), None);
    }

    /// A ZFS child dataset of the folder's mount, mounted at the entry, is
    /// one the walk enters; a bind mount of its mount point is another
    /// mount; `.zfs` is told only at a dataset's root.
    #[test]
    fn mountinfo_tells_a_zfs_child_dataset_from_a_bind_of_it() {
        let lines = super::super::statfs::parse(
            b"\
70 22 0:60 / /tank rw - zfs tank rw
71 70 0:61 / /tank/home rw - zfs tank/home rw
73 70 0:61 / /tank/bound rw - zfs tank/home rw
",
        );
        let mut m = Mounts { by_id: Some(lines.into_iter().map(|l| (l.id, l)).collect()), points: None };
        let marked = |id| Seen { mnt_id: Some(id), told: true };
        let edge = |m: &mut Mounts, id, name: &str| {
            m.boundary(Some(Boundary::Mount), marked(id), Path::new("/tank"), OsStr::new(name), Some(70))
        };
        assert_eq!(edge(&mut m, 71, "home"), Some(Boundary::Dataset(71)));
        assert_eq!(edge(&mut m, 73, "bound"), Some(Boundary::Mount), "a bind of its mount point");
        assert_eq!(edge(&mut m, 71, "elsewhere"), Some(Boundary::Mount), "not mounted at the entry");
        let unasked = || -> Option<u32> { panic!("mountinfo told") };
        assert!(m.zfs_root(Some(70), Path::new("/tank"), None, unasked), "by mountinfo");
        assert!(m.zfs_root(Some(70), Path::new("/x"), Some(true), unasked), "by the kernel");
        assert!(!m.zfs_root(Some(70), Path::new("/tank/sub"), Some(false), unasked));
    }

    /// Without mountinfo's line, `.zfs` is the snapshot folder of any ZFS
    /// folder the kernel does not deny is a mount root: an unknown answer
    /// fails closed.
    #[test]
    fn without_mountinfo_a_zfs_folder_not_denied_a_mount_root_hides_its_snapshots() {
        let zfs = || Some(ZFS_SUPER_MAGIC);
        let dir = Path::new("/tank");
        assert!(zfs_root(None, dir, Some(true), zfs), "a mount root");
        assert!(zfs_root(None, dir, None, zfs), "the kernel does not tell: closed");
        assert!(!zfs_root(None, dir, Some(false), zfs), "the kernel says not a root");
        assert!(!zfs_root(None, dir, None, || Some(0xEF53)), "ext4");
        assert!(zfs_root(None, dir, None, || None), "no statfs either: closed");
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

    /// Without `STATX_MNT_ID` the mount id is fdinfo's (safety ADR,
    /// amendment of 2026-11-01, completed on 2026-11-02): the line is read
    /// as the kernel writes it, and both answers agree where both exist.
    #[test]
    fn the_fdinfo_mount_id_is_the_statx_one() {
        assert_eq!(fdinfo_mnt_id(b"pos:\t0\nflags:\t02100000\nmnt_id:\t31\nino:\t2\n"), Some(31));
        assert_eq!(fdinfo_mnt_id(b"pos:\t0\nflags:\t02100000\n"), None, "below 3.15");
        let d = tempfile::tempdir().unwrap();
        let fd =
            rustix::fs::open(d.path(), OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty()).unwrap();
        let statx = stat(&fd, c"", AtFlags::EMPTY_PATH, false).unwrap().mnt_id;
        let info = fdinfo_mnt_id(&std::fs::read(format!("/proc/self/fdinfo/{}", fd.as_raw_fd())).unwrap());
        assert!(info.is_some(), "fdinfo tells");
        if statx.is_some() {
            assert_eq!(mount_id(&fd, None), statx);
        }
    }

    /// The guard's open is strict: with no mount id for the root, a folder
    /// opened by path is refused, where the scan reads on with `dev`.
    #[test]
    fn without_a_mount_id_the_guards_open_refuses_and_the_scans_reads_on() {
        let d = tempfile::tempdir().unwrap();
        let root = crate::platform::Canonical::canonical(d.path()).unwrap();
        fs::create_dir(root.join("a")).unwrap();
        let untold = Anchor { mnt_id: None, ..anchor_by_path(&root).unwrap() };
        assert!(open_in(&untold, &root.join("a"), false).is_ok());
        assert_eq!(open_in(&untold, &root.join("a"), true).unwrap_err().kind, ReadErrorKind::OtherFilesystem);
        let told = anchor_by_path(&root).unwrap();
        assert!(open_in(&told, &root.join("a"), true).is_ok(), "fdinfo or statx tells it");
    }

    /// A root renamed away and replaced since the guard anchored it: its
    /// path names another folder now, and the guard's open refuses it.
    #[test]
    fn a_root_replaced_since_the_anchor_is_refused_by_the_guards_open() {
        let d = tempfile::tempdir().unwrap();
        let base = crate::platform::Canonical::canonical(d.path()).unwrap();
        let root = base.join("root");
        fs::create_dir_all(root.join("a")).unwrap();
        for (n, held) in [anchor(&root).unwrap(), anchor_by_path(&root).unwrap()].into_iter().enumerate() {
            assert!(open_in(&held, &root.join("a"), true).is_ok());
            fs::rename(&root, base.join("old")).unwrap();
            fs::create_dir_all(root.join("a")).unwrap();
            assert_eq!(open_in(&held, &root.join("a"), true).unwrap_err().kind, ReadErrorKind::Changed);
            assert!(open_in(&held, &root.join("a"), false).is_ok(), "the scan's open is not the guard's");
            fs::rename(&root, base.join(format!("new-{n}"))).unwrap();
            fs::rename(base.join("old"), &root).unwrap();
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
