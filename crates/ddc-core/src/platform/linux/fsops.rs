//! Directory operations relative to a verified parent descriptor, so the
//! executor acts on exactly the directory it checked (no path
//! re-resolution), on Linux (safety ADR §5, amendment of 2026-10-31).

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rustix::fs::{Access, AtFlags, CWD, Mode, OFlags, RenameFlags, Timespec, Timestamps, UTIME_OMIT};
use rustix::io::Errno;

use crate::platform::types::EntryStat;

/// Sets the modification time of `path`, without following a symlink, and
/// leaves its access time. For tests: an entry written by a test is
/// "recent". Linux keeps no date added, so `added` is ignored.
pub fn set_times(path: &Path, modified: SystemTime, _added: SystemTime) -> io::Result<()> {
    let since = modified.duration_since(UNIX_EPOCH).unwrap_or_default();
    let times = Timestamps {
        last_access: Timespec { tv_sec: 0, tv_nsec: UTIME_OMIT },
        last_modification: Timespec {
            tv_sec: i64::try_from(since.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: since.subsec_nanos().into(),
        },
    };
    Ok(rustix::fs::utimensat(CWD, path, &times, AtFlags::SYMLINK_NOFOLLOW)?)
}

/// Opens `path` to read, without following a symlink or blocking on a FIFO
/// (`O_NOFOLLOW | O_NONBLOCK`).
pub fn open_nofollow_nonblock(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let flags = (OFlags::NOFOLLOW | OFlags::NONBLOCK).bits();
    std::fs::OpenOptions::new().read(true).custom_flags(i32::try_from(flags).unwrap_or(0)).open(path)
}

/// Opens `path` to read a file a walk found (a marker): only a regular file,
/// never through a symlink (`O_NOFOLLOW`), never blocking on a FIFO
/// (`O_NONBLOCK`), never taking a terminal (`O_NOCTTY`). The descriptor is
/// `fstat`ed before anything is read: a FIFO, a socket or a device is an
/// error, closed unread (safety ADR, amendment of 2026-11-04).
pub fn open_read(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let flags = (OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY).bits();
    let file = std::fs::OpenOptions::new().read(true).custom_flags(i32::try_from(flags).unwrap_or(0)).open(path)?;
    crate::platform::regular_only(file)
}

/// Clears read-only attributes below `root` before a tree delete is tried
/// again: on Windows, for FAT and exFAT (story "Windows scanner and rule
/// catalog", step 8b). Nothing to do on Linux: a file's mode never stops its
/// removal, its folder's does, and `make_dirs_writable` opens those.
pub fn clear_read_only(_root: &Path) -> io::Result<()> {
    Ok(())
}

/// The user running ddc may write in `path`, as `access(2)` with `W_OK`
/// answers: the mode bits, the ACLs and a read-only mount. `false` when it
/// does not exist.
pub fn can_write(path: &Path) -> bool {
    rustix::fs::access(path, Access::WRITE_OK).is_ok()
}

/// An open directory, opened without following a final symlink.
#[derive(Debug)]
pub struct DirFd(OwnedFd);

impl DirFd {
    pub fn open(path: &Path) -> io::Result<Self> {
        let fd = rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )?;
        Ok(Self(fd))
    }

    /// `dir`, at or beneath `anchor`'s root, opened as the walk opens it
    /// (`super::bulk::open_in`) and stricter: a mount crossed or a
    /// symlink on the way, or no mount id to tell, is refused.
    pub fn open_beneath(anchor: &super::bulk::Anchor, dir: &Path) -> io::Result<Self> {
        match super::bulk::open_in(anchor, dir, true) {
            Ok(fd) => Ok(Self(fd)),
            Err(e) => {
                Err(e.errno.map_or_else(|| io::Error::other(format!("{:?}", e.kind)), io::Error::from_raw_os_error))
            }
        }
    }

    /// The open descriptor, for the Trash's move out of this folder.
    pub(super) fn fd(&self) -> &OwnedFd {
        &self.0
    }

    /// `(dev, ino)` of the directory itself, its device the reported one
    /// (`super::fsid`).
    pub fn identity(&self) -> io::Result<(u64, u64)> {
        Ok(super::bulk::held(&self.0)?.id)
    }

    pub fn stat_child(&self, name: &OsStr) -> io::Result<EntryStat> {
        Ok(super::bulk::stat_child(&self.0, name)?)
    }

    /// The child's modification time, Unix seconds, without following a
    /// symlink. Linux keeps no date added: always `None`.
    pub fn child_times(&self, name: &OsStr) -> io::Result<(Option<i64>, Option<i64>)> {
        let st = rustix::fs::statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW)?;
        Ok((Some(st.st_mtime), None))
    }

    /// Renames `from` to `to` inside this directory; fails if `to` exists.
    /// `renameat2(RENAME_NOREPLACE)` makes that one operation; a filesystem
    /// that refuses the flag (`EINVAL`) gets the check, then `renameat`.
    pub fn rename_child(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        match rustix::fs::renameat_with(&self.0, from, &self.0, to, RenameFlags::NOREPLACE) {
            Err(Errno::INVAL) => {
                if self.stat_child(to).is_ok() {
                    return Err(io::Error::from(io::ErrorKind::AlreadyExists));
                }
                Ok(rustix::fs::renameat(&self.0, from, &self.0, to)?)
            }
            done => Ok(done?),
        }
    }

    /// Removes the folder `name` of this directory and everything in it,
    /// through descriptors, never crossing a mount ([`super::bulk::remove_tree`]).
    /// `path` names it in an error alone. Only `exec::delete` calls it.
    pub fn remove_tree(&self, name: &OsStr, _path: &Path) -> io::Result<()> {
        super::bulk::remove_tree(&self.0, name)
    }

    /// Unlinks the child `name` of this directory (`unlinkat` without
    /// `AT_REMOVEDIR`, so never a folder). The only removal outside the
    /// executor: `ddc keep remove` calls it once, for `.ddc-keep`, after
    /// [`Self::stat_child`] said it is a regular file (safety ADR, amendment
    /// of 2026-10-30).
    pub fn unlink_child(&self, name: &OsStr) -> io::Result<()> {
        Ok(rustix::fs::unlinkat(&self.0, name, AtFlags::empty())?)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use super::*;
    use crate::platform::{meta, testing};

    #[test]
    fn the_directory_and_its_children_are_stated_as_std_states_them() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("f"), vec![1u8; 5000]).unwrap();
        fs::create_dir(d.path().join("sub")).unwrap();
        testing::symlink(Path::new("/"), &d.path().join("sym")).unwrap();
        let dir = DirFd::open(d.path()).unwrap();

        assert_eq!(dir.identity().unwrap(), meta::lstat(d.path()).unwrap().id());
        let f = dir.stat_child(OsStr::new("f")).unwrap();
        let st = meta::lstat(&d.path().join("f")).unwrap();
        assert_eq!((f.dev, f.ino, f.alloc), (st.dev, st.ino, st.alloc));
        assert!(f.is_file && !f.is_dir && !f.is_symlink);
        assert!(dir.stat_child(OsStr::new("sub")).unwrap().is_dir);
        assert!(dir.stat_child(OsStr::new("sym")).unwrap().is_symlink, "never followed");
        assert_eq!(dir.stat_child(OsStr::new("gone")).unwrap_err().kind(), io::ErrorKind::NotFound);
    }

    /// The guard's parent opens beneath its root, with `openat2` and
    /// without: a path outside the root, or (with `openat2`) a symlink on
    /// the way, is refused; a child on the same mount is no mount root.
    #[test]
    fn a_parent_opens_beneath_its_root_and_never_through_a_symlink() {
        use crate::platform::bulk;
        let d = tempfile::tempdir().unwrap();
        let root = crate::platform::Canonical::canonical(d.path()).unwrap();
        fs::create_dir_all(root.join("a/b")).unwrap();
        testing::symlink(&root.join("a"), &root.join("via")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        for (anchor, beneath) in [(bulk::anchor(&root).unwrap(), true), (bulk::anchor_by_path(&root).unwrap(), false)] {
            let dir = DirFd::open_beneath(&anchor, &root.join("a")).unwrap();
            assert!(!dir.stat_child(OsStr::new("b")).unwrap().mount_root, "same mount");
            assert!(DirFd::open_beneath(&anchor, outside.path()).is_err(), "outside the root");
            assert!(DirFd::open_beneath(&anchor, &root.join("via")).is_err(), "the symlink itself");
            if beneath {
                assert!(DirFd::open_beneath(&anchor, &root.join("via/b")).is_err(), "a symlink on the way");
            }
        }
    }

    #[test]
    fn a_symlink_is_never_opened_as_the_directory() {
        let d = tempfile::tempdir().unwrap();
        testing::symlink(d.path(), &d.path().join("sym")).unwrap();
        assert!(DirFd::open(&d.path().join("sym")).is_err());
        assert!(open_nofollow_nonblock(&d.path().join("sym")).is_err());
    }

    #[test]
    fn a_rename_never_replaces_and_an_unlink_never_removes_a_folder() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("a")).unwrap();
        fs::create_dir(d.path().join("taken")).unwrap();
        fs::write(d.path().join("file"), b"x").unwrap();
        let dir = DirFd::open(d.path()).unwrap();

        let refused = dir.rename_child(OsStr::new("a"), OsStr::new("taken")).unwrap_err();
        assert_eq!(refused.kind(), io::ErrorKind::AlreadyExists);
        assert!(d.path().join("a").is_dir() && d.path().join("taken").is_dir());
        dir.rename_child(OsStr::new("a"), OsStr::new("b")).unwrap();
        assert!(d.path().join("b").is_dir() && !d.path().join("a").exists());

        assert!(dir.unlink_child(OsStr::new("b")).is_err(), "a folder is never unlinked");
        dir.unlink_child(OsStr::new("file")).unwrap();
        assert!(!d.path().join("file").exists());
    }

    #[test]
    fn set_times_sets_the_modification_time_of_the_link_itself() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("f");
        fs::write(&f, b"x").unwrap();
        let then = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        set_times(&f, then, then).unwrap();
        assert_eq!(meta::mtime(&fs::symlink_metadata(&f).unwrap()), 1_700_000_000);
        let dir = DirFd::open(d.path()).unwrap();
        assert_eq!(dir.child_times(OsStr::new("f")).unwrap(), (Some(1_700_000_000), None));
        assert!(can_write(d.path()));
        assert!(!can_write(&d.path().join("gone")));
    }
}
