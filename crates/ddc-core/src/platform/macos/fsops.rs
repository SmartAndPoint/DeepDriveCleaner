//! Directory operations relative to a verified parent descriptor, so the
//! executor acts on exactly the directory it checked (no path re-resolution).

use std::ffi::{CString, OsStr};
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use libc::{c_char, c_int, c_ulong, c_void, size_t};

use crate::platform::types::EntryStat;

use super::bulk::{ATTR_BIT_MAP_COUNT, ATTR_CMN_ADDEDTIME, ATTR_CMN_MODTIME, ATTR_CMN_RETURNED_ATTRS, AttrList};

const FSOPT_NOFOLLOW: c_ulong = 0x0000_0001;

unsafe extern "C" {
    fn getattrlistat(
        fd: c_int,
        path: *const c_char,
        attr_list: *mut AttrList,
        attr_buf: *mut c_void,
        attr_buf_size: size_t,
        options: c_ulong,
    ) -> c_int;
    fn setattrlist(
        path: *const c_char,
        attr_list: *mut AttrList,
        attr_buf: *mut c_void,
        attr_buf_size: size_t,
        options: c_ulong,
    ) -> c_int;
}

fn times_list() -> AttrList {
    AttrList {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: ATTR_CMN_MODTIME | ATTR_CMN_ADDEDTIME,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    }
}

/// Sets the modification time and the date added of `path`, without
/// following a symlink. For tests: an entry written by a test is "recent".
pub fn set_times(path: &Path, modified: SystemTime, added: SystemTime) -> io::Result<()> {
    let secs = |t: SystemTime| t.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let mut buf = [0u8; 32];
    buf[..8].copy_from_slice(&secs(modified).to_ne_bytes());
    buf[16..24].copy_from_slice(&secs(added).to_ne_bytes());
    let c = cstring(path.as_os_str().as_bytes())?;
    let mut list = times_list();
    // SAFETY: NUL-terminated path, valid attrlist, `buf` holds both timespecs in bit order.
    if unsafe { setattrlist(c.as_ptr(), &mut list, buf.as_mut_ptr().cast(), buf.len(), FSOPT_NOFOLLOW) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// An open directory, opened without following a final symlink.
#[derive(Debug)]
pub struct DirFd(c_int);

impl Drop for DirFd {
    fn drop(&mut self) {
        // SAFETY: we own the descriptor and close it once.
        unsafe { libc::close(self.0) };
    }
}

/// Opens `path` to read, without following a symlink or blocking on a FIFO
/// (`O_NOFOLLOW | O_NONBLOCK`).
pub fn open_nofollow_nonblock(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(path)
}

/// The user running ddc may write in `path`, as `access(2)` with `W_OK`
/// answers: the mode bits, the ACLs and a read-only volume. `false` when
/// it does not exist.
pub fn can_write(path: &Path) -> bool {
    let Ok(c) = cstring(path.as_os_str().as_bytes()) else { return false };
    // SAFETY: NUL-terminated path; `access` reads it and writes nothing.
    unsafe { libc::access(c.as_ptr(), libc::W_OK) == 0 }
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
            is_file: fmt == libc::S_IFREG,
            is_symlink: fmt == libc::S_IFLNK,
            flags: st.st_flags,
            alloc: u64::try_from(st.st_blocks).unwrap_or(0) * 512,
        })
    }

    /// The child's modification time and date added, Unix seconds, without
    /// following a symlink; `None` for what the filesystem does not keep.
    pub fn child_times(&self, name: &OsStr) -> io::Result<(Option<i64>, Option<i64>)> {
        let c = cstring(name.as_bytes())?;
        let mut list = times_list();
        list.commonattr |= ATTR_CMN_RETURNED_ATTRS;
        // Length, the returned attribute set (5 × u32), two timespecs.
        let mut buf = [0u8; 4 + 20 + 32];
        // SAFETY: open fd, NUL-terminated name, valid attrlist, `buf` writable for its length.
        if unsafe { getattrlistat(self.0, c.as_ptr(), &mut list, buf.as_mut_ptr().cast(), buf.len(), FSOPT_NOFOLLOW) }
            != 0
        {
            return Err(io::Error::last_os_error());
        }
        let word = |at: usize| u32::from_ne_bytes(buf[at..at + 4].try_into().expect("4 bytes"));
        let secs = |at: usize| i64::from_ne_bytes(buf[at..at + 8].try_into().expect("8 bytes"));
        let returned = word(4);
        let mut at = 24;
        let modified = (returned & ATTR_CMN_MODTIME != 0).then(|| {
            at += 16;
            secs(at - 16)
        });
        let added = (returned & ATTR_CMN_ADDEDTIME != 0).then(|| secs(at));
        Ok((modified, added))
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
