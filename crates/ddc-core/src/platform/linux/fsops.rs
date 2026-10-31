//! Directory operations relative to a verified parent descriptor, so the
//! executor acts on exactly the directory it checked (no path
//! re-resolution), on Linux (safety ADR §5, amendment of 2026-10-31).

use std::ffi::OsStr;
use std::fs::File;
use std::io;
use std::os::fd::OwnedFd;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rustix::fs::{Access, AtFlags, CWD, FileType, Mode, OFlags, RenameFlags, Timespec, Timestamps, UTIME_OMIT};
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

    /// `(dev, ino)` of the directory itself.
    pub fn identity(&self) -> io::Result<(u64, u64)> {
        let st = rustix::fs::fstat(&self.0)?;
        Ok((st.st_dev, st.st_ino))
    }

    pub fn stat_child(&self, name: &OsStr) -> io::Result<EntryStat> {
        let st = rustix::fs::statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW)?;
        let ft = FileType::from_raw_mode(st.st_mode);
        Ok(EntryStat {
            dev: st.st_dev,
            ino: st.st_ino,
            is_dir: ft == FileType::Directory,
            is_file: ft == FileType::RegularFile,
            is_symlink: ft == FileType::Symlink,
            flags: 0,
            alloc: u64::try_from(st.st_blocks).unwrap_or(0) * 512,
        })
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

        assert_eq!(dir.identity().unwrap(), meta::file_id(&fs::symlink_metadata(d.path()).unwrap()).unwrap());
        let f = dir.stat_child(OsStr::new("f")).unwrap();
        let md = fs::symlink_metadata(d.path().join("f")).unwrap();
        assert_eq!(
            (f.dev, f.ino, f.alloc),
            (meta::file_id(&md).unwrap().0, meta::file_id(&md).unwrap().1, meta::alloc_bytes(&md))
        );
        assert!(f.is_file && !f.is_dir && !f.is_symlink);
        assert!(dir.stat_child(OsStr::new("sub")).unwrap().is_dir);
        assert!(dir.stat_child(OsStr::new("sym")).unwrap().is_symlink, "never followed");
        assert_eq!(dir.stat_child(OsStr::new("gone")).unwrap_err().kind(), io::ErrorKind::NotFound);
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
