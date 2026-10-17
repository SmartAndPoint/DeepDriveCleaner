//! Directory operations relative to a verified parent descriptor, so the
//! executor acts on exactly the directory it checked (no path re-resolution).

use std::ffi::{CString, OsStr};
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;

use libc::c_int;

/// An open directory, opened without following a final symlink.
#[derive(Debug)]
pub struct DirFd(c_int);

impl Drop for DirFd {
    fn drop(&mut self) {
        // SAFETY: we own the descriptor and close it once.
        unsafe { libc::close(self.0) };
    }
}

/// Identity and type of an entry, from `fstatat(.., AT_SYMLINK_NOFOLLOW)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryStat {
    pub dev: u64,
    pub ino: u64,
    pub is_dir: bool,
    pub is_symlink: bool,
}

fn cstring(bytes: &[u8]) -> io::Result<CString> {
    CString::new(bytes).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))
}

impl DirFd {
    pub fn open(path: &Path) -> io::Result<Self> {
        let c = cstring(path.as_os_str().as_bytes())?;
        // SAFETY: `c` is NUL-terminated.
        let fd =
            unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
        if fd < 0 { Err(io::Error::last_os_error()) } else { Ok(Self(fd)) }
    }

    /// `(dev, ino)` of the directory itself.
    pub fn identity(&self) -> io::Result<(u64, u64)> {
        let mut st = MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: open fd, valid out-pointer.
        if unsafe { libc::fstat(self.0, st.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fstat succeeded.
        let st = unsafe { st.assume_init() };
        Ok((st.st_dev as u64, st.st_ino))
    }

    pub fn stat_child(&self, name: &OsStr) -> io::Result<EntryStat> {
        let c = cstring(name.as_bytes())?;
        let mut st = MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: open fd, NUL-terminated name, valid out-pointer.
        if unsafe { libc::fstatat(self.0, c.as_ptr(), st.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fstatat succeeded.
        let st = unsafe { st.assume_init() };
        let fmt = st.st_mode & libc::S_IFMT;
        Ok(EntryStat {
            dev: st.st_dev as u64,
            ino: st.st_ino,
            is_dir: fmt == libc::S_IFDIR,
            is_symlink: fmt == libc::S_IFLNK,
        })
    }

    /// Renames `from` to `to` inside this directory; fails if `to` exists.
    pub fn rename_child(&self, from: &OsStr, to: &OsStr) -> io::Result<()> {
        if self.stat_child(to).is_ok() {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        let (f, t) = (cstring(from.as_bytes())?, cstring(to.as_bytes())?);
        // SAFETY: open fd used for both sides, NUL-terminated names.
        if unsafe { libc::renameat(self.0, f.as_ptr(), self.0, t.as_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}
