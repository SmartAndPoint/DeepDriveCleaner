//! The device a file reports (safety ADR, amendment of 2026-11-02, "btrfs
//! and ZFS on Linux", "Stable identities"). btrfs and ZFS give a subvolume
//! or a dataset an anonymous `st_dev`, which changes with mount order and
//! boot; there the reported device is the filesystem's `f_fsid`, folded
//! into 64 bits, which stays: btrfs derives it from the on-disk fsid and
//! the subvolume's root id, ZFS from the dataset's fsid guid. Every other
//! filesystem reports `st_dev`, exactly. Every producer of a device asks
//! here, or a saved identity, or the process table's, stops matching.

use std::collections::BTreeMap;
use std::fs::Metadata;
use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::fs::MetadataExt as _;
use std::path::Path;
use std::sync::Mutex;

use rustix::fs::{Mode, OFlags};
use rustix::io::Errno;

/// `statfs`'s `f_type` of btrfs (`BTRFS_SUPER_MAGIC`).
pub(super) const BTRFS_SUPER_MAGIC: u32 = 0x9123_683E;

/// `statfs`'s `f_type` of ZFS (OpenZFS `ZFS_SUPER_MAGIC`).
pub(super) const ZFS_SUPER_MAGIC: u32 = 0x2FC1_2FC1;

/// Linux's `PATH_MAX`, the terminating NUL included.
const PATH_MAX: usize = 4096;

/// The device reported for each mount id and raw `st_dev` this process has
/// met, so `fstatfs`, a round trip to the server on NFS, runs once for
/// each. The known limit: a subvolume deleted and made again in the same
/// mount can take the old one's anonymous device within one process, and
/// read the old one's fold. Its files then look changed since the scan, or
/// the same as files the scan saw at the same inode, as the raw `st_dev`
/// would make them anyway: a stale scan, never a protection lifted, which
/// the `.ddc-keep` marker, checked by path, carries.
static FOLDED: Mutex<BTreeMap<(u64, u64), u64>> = Mutex::new(BTreeMap::new());

/// The device an open file reports, `raw` being its `st_dev` and `mnt` its
/// mount id ([`super::bulk::held`]): from [`FOLDED`], or else one
/// `fstatfs`, which an `O_PATH` descriptor allows, and on btrfs and ZFS one
/// `fstatvfs`. With no mount id (a kernel before 5.8 with no `/proc`, so no
/// mountinfo to tell the type either) `fstatfs` runs every time. An error
/// is returned, never the raw device in its place.
pub(super) fn of_fd(fd: impl AsFd, raw: u64, mnt: Option<u64>) -> Result<u64, Errno> {
    let Some(mnt) = mnt else { return fold(&fd, raw) };
    if let Some(&dev) = FOLDED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&(mnt, raw)) {
        return Ok(dev);
    }
    let dev = fold(&fd, raw)?;
    FOLDED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert((mnt, raw), dev);
    Ok(dev)
}

/// [`of_fd`] asked of the filesystem. rustix keeps `fstatfs`'s `f_fsid`
/// halves private and gives them folded through `fstatvfs`: the first the
/// low half, the second the high one, as the kernel's `statvfs` reads them.
fn fold(fd: impl AsFd, raw: u64) -> Result<u64, Errno> {
    let st = super::bulk::retry(|| rustix::fs::fstatfs(&fd))?;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a magic number fits 32 bits
    let magic = st.f_type as u32;
    if !folds(magic) {
        return Ok(raw);
    }
    Ok(super::bulk::retry(|| rustix::fs::fstatvfs(&fd))?.f_fsid)
}

/// The device the file `md` describes reports, read through `path`, which
/// is opened `O_PATH` (no automount; a final symlink followed only when
/// `follow`) and refused unless it is still that file. A path past
/// `PATH_MAX`, which only a listing reaches, opens as its folder, then the
/// name in it.
pub(super) fn of_path(path: &Path, follow: bool, md: &Metadata) -> io::Result<u64> {
    let flags = OFlags::PATH | OFlags::CLOEXEC | if follow { OFlags::empty() } else { OFlags::NOFOLLOW };
    let fd: OwnedFd = if path.as_os_str().len() < PATH_MAX {
        super::bulk::retry(|| rustix::fs::open(path, flags, Mode::empty()))?
    } else {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(Errno::NAMETOOLONG.into());
        };
        let dir = super::bulk::retry(|| {
            rustix::fs::open(dir, OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty())
        })?;
        super::bulk::retry(|| rustix::fs::openat(&dir, name, flags, Mode::empty()))?
    };
    let held = super::bulk::held(&fd)?;
    if (held.raw, held.id.1) != (md.dev(), md.ino()) {
        return Err(io::Error::other(format!("{} changed while it was read", path.display())));
    }
    Ok(held.id.0)
}

/// The filesystem of `statfs`'s `f_type` `magic` reports its `f_fsid`:
/// btrfs and ZFS.
fn folds(magic: u32) -> bool {
    matches!(magic, BTRFS_SUPER_MAGIC | ZFS_SUPER_MAGIC)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;

    use super::*;
    use crate::agents::liveness::{self, FileFact, LiveBy, SessionFacts, SessionLiveness};
    use crate::platform::types::Times;
    use crate::platform::{bulk, fsops::DirFd, meta, procs};

    /// Only btrfs and ZFS report `f_fsid`: ext4, tmpfs, XFS, NFS and the
    /// rest keep `st_dev`.
    #[test]
    fn only_btrfs_and_zfs_report_the_fsid() {
        assert!(folds(BTRFS_SUPER_MAGIC) && folds(ZFS_SUPER_MAGIC));
        for other in [0xEF53, 0x0102_1994, 0x5846_5342, 0x6969, 0xFE53_4D42, 0x0187] {
            assert!(!folds(other), "{other:#x}");
        }
    }

    /// Every producer gives a file and its folder one device: the listing
    /// and its entries, the guard's open folder and its child, `lstat`,
    /// `stat` and a listed entry, the Trash's, and the process table's for
    /// a file this process holds open, which makes the guard call a session
    /// of it live. Here on the test's own filesystem;
    /// a btrfs subvolume and a ZFS dataset, where the device is folded, in
    /// the linux-mounts job.
    #[test]
    fn every_producer_gives_one_device() {
        let d = tempfile::tempdir().unwrap();
        let (dir, file) = (d.path(), d.path().join("f"));
        fs::write(&file, b"x").unwrap();
        let held = fs::File::open(&file).unwrap();

        let want = meta::lstat(&file).unwrap().id();
        let folder = meta::lstat(dir).unwrap().id();
        assert_eq!(folder.0, want.0, "one filesystem");
        let listing = bulk::list(dir, Some(folder), false, Times::None).unwrap();
        assert_eq!((listing.dev, listing.ino), folder);
        let entry = listing.entries.iter().find(|e| e.name == "f").unwrap();
        assert_eq!((entry.dev, entry.ino), want);
        let open = DirFd::open(dir).unwrap();
        assert_eq!(open.identity().unwrap(), folder);
        let child = open.stat_child(OsStr::new("f")).unwrap();
        assert_eq!((child.dev, child.ino), want);
        assert_eq!(meta::stat(&file).unwrap().id(), want);
        let listed = crate::stats::read_dir(dir).unwrap().flatten().find(|e| e.file_name() == "f").unwrap();
        assert_eq!(meta::entry(&listed).unwrap().id(), want);
        assert_eq!(bulk::fd_mount(open.fd()).unwrap().0, folder);
        let me = i32::try_from(std::process::id()).unwrap();
        assert!(procs::open_vnodes(me).iter().any(|v| (v.dev, v.ino) == want), "held open");
        let session = SessionFacts { session: "s".into(), files: vec![FileFact { id: want, modified: Some(0) }] };
        let verdict =
            liveness::claude(Ok(&[][..]), &[session], &[me], procs::open_vnodes, std::time::SystemTime::now());
        assert_eq!(verdict, [SessionLiveness::Live { by: LiveBy::OpenFile }], "the guard's verdict");
        let raw = fs::symlink_metadata(&file).unwrap();
        assert_eq!(of_path(&file, false, &raw).unwrap(), want.0);
        drop(held);
    }

    /// `fstatfs` runs once for each mount and device: a pair met before is
    /// answered from the cache, not asked of the filesystem again.
    #[test]
    fn a_mount_and_device_met_before_are_answered_from_the_cache() {
        let d = tempfile::tempdir().unwrap();
        let fd = rustix::fs::open(d.path(), OFlags::PATH | OFlags::CLOEXEC, Mode::empty()).unwrap();
        FOLDED.lock().unwrap().insert((u64::MAX, 7), 42);
        assert_eq!(of_fd(&fd, 7, Some(u64::MAX)).unwrap(), 42);
    }

    /// A path that names another file by the time it is opened is refused,
    /// never given the other file's device.
    #[test]
    fn a_path_swapped_since_its_stat_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (d.path().join("a"), d.path().join("b"));
        fs::write(&a, b"a").unwrap();
        fs::write(&b, b"b").unwrap();
        let md = fs::symlink_metadata(&a).unwrap();
        fs::rename(&b, &a).unwrap();
        assert!(of_path(&a, false, &md).is_err());
        assert_eq!(of_path(&a, false, &fs::symlink_metadata(&a).unwrap()).unwrap(), meta::lstat(&a).unwrap().dev);
    }
}
