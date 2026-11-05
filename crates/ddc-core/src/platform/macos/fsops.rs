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

/// Opens `path` to read a file a walk found (a marker): only a regular file,
/// never through a symlink (`O_NOFOLLOW`), never blocking on a FIFO
/// (`O_NONBLOCK`), never taking a terminal (`O_NOCTTY`). The descriptor is
/// `fstat`ed before anything is read: a FIFO, a socket or a device is an
/// error, closed unread (safety ADR, amendment of 2026-11-04). A dataless
/// file is never materialised: the process disables that at start
/// ([`super::iopolicy`]).
pub fn open_read(path: &Path) -> io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)?;
    crate::platform::regular_only(file)
}

/// Clears read-only attributes below `root` before a tree delete is tried
/// again: on Windows, for FAT and exFAT (story "Windows scanner and rule
/// catalog", step 8b). Nothing to do on macOS: a file's mode never stops its
/// removal, its folder's does, and `make_dirs_writable` opens those.
pub fn clear_read_only(_root: &Path) -> io::Result<()> {
    Ok(())
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

    /// `dir`, at or beneath `anchor`'s root, opened as [`Self::open`] opens
    /// it: a volume is a device here, which the caller's `(dev, ino)`
    /// checks tell.
    pub fn open_beneath(anchor: &super::bulk::Anchor, dir: &Path) -> io::Result<Self> {
        if !dir.starts_with(&anchor.path) {
            return Err(io::Error::other("outside the scan root"));
        }
        Self::open(dir)
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
            mount_root: false,
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

    /// Removes the folder `name` of this directory and everything in it,
    /// through descriptors, never by path (safety ADR §5, amendment of
    /// 2026-11-04, "removal never crosses a mount"). Each folder opens from
    /// its parent's descriptor (`openat` with `O_NOFOLLOW | O_DIRECTORY`),
    /// and its `fstat` must give its parent's device: a volume mounted
    /// inside since the guard's check stops the removal with an error, and
    /// what lies beyond stays. So does running out of descriptors, one open
    /// per level. A folder of the user's own is made `u+rwx` on its own
    /// descriptor before anything in it is removed (Go's module cache is
    /// read-only). `path` names it in an error alone. Only `exec::delete`
    /// calls it.
    pub fn remove_tree(&self, name: &OsStr, _path: &Path) -> io::Result<()> {
        let me = crate::platform::procs::uid();
        let dev = self.identity()?.0;
        let mut stack = vec![doomed(self.0, cstring(name.as_bytes())?, dev, me)?];
        while let Some(last) = stack.last_mut() {
            match last.left.pop() {
                Some((child, is_dir)) => {
                    let is_dir = match is_dir {
                        Some(is_dir) => is_dir,
                        None => {
                            let mut st = MaybeUninit::<libc::stat>::zeroed();
                            // SAFETY: open fd, NUL-terminated name, valid out-pointer.
                            if unsafe {
                                libc::fstatat(last.fd.0, child.as_ptr(), st.as_mut_ptr(), libc::AT_SYMLINK_NOFOLLOW)
                            } != 0
                            {
                                return Err(io::Error::last_os_error());
                            }
                            // SAFETY: fstatat succeeded.
                            unsafe { st.assume_init() }.st_mode & libc::S_IFMT == libc::S_IFDIR
                        }
                    };
                    if is_dir {
                        let next = doomed(last.fd.0, child, last.dev, me)?;
                        stack.push(next);
                    } else {
                        // SAFETY: open fd, NUL-terminated name.
                        if unsafe { libc::unlinkat(last.fd.0, child.as_ptr(), 0) } != 0 {
                            return Err(io::Error::last_os_error());
                        }
                    }
                }
                None => {
                    let Doomed { fd, name, .. } = stack.pop().expect("the last level");
                    drop(fd);
                    let above = stack.last().map_or(self.0, |l| l.fd.0);
                    // SAFETY: open fd, NUL-terminated name.
                    if unsafe { libc::unlinkat(above, name.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
            }
        }
        Ok(())
    }

    /// Unlinks the child `name` of this directory (`unlinkat` without
    /// `AT_REMOVEDIR`, so never a folder). The only removal outside the
    /// executor: `ddc keep remove` calls it once, for `.ddc-keep`, after
    /// [`Self::stat_child`] said it is a regular file (safety ADR, amendment
    /// of 2026-10-30).
    pub fn unlink_child(&self, name: &OsStr) -> io::Result<()> {
        let c = cstring(name.as_bytes())?;
        // SAFETY: open fd, NUL-terminated name.
        if unsafe { libc::unlinkat(self.0, c.as_ptr(), 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

/// One folder of a tree being removed ([`DirFd::remove_tree`]).
struct Doomed {
    fd: DirFd,
    /// Its name in the folder above.
    name: CString,
    dev: u64,
    /// What is left to remove in it, each with whether it is a folder
    /// (`None`: `DT_UNKNOWN`, to be `fstatat`ed).
    left: Vec<(CString, Option<bool>)>,
}

/// The folder `name` of `parent`, opened for [`DirFd::remove_tree`] and
/// listed, or why it must not be entered.
fn doomed(parent: c_int, name: CString, dev: u64, me: u32) -> io::Result<Doomed> {
    // SAFETY: open fd, NUL-terminated name.
    let raw = unsafe {
        libc::openat(parent, name.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let fd = DirFd(raw);
    let mut st = MaybeUninit::<libc::stat>::zeroed();
    // SAFETY: open fd, valid out-pointer.
    if unsafe { libc::fstat(fd.0, st.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstat succeeded.
    let st = unsafe { st.assume_init() };
    if st.st_dev as u64 != dev {
        return Err(io::Error::other(format!("another volume at {}, left in place", name.to_string_lossy())));
    }
    // Another user's folder is never changed (safety ADR, amendment of 2026-10-31).
    if st.st_uid == me && st.st_mode & 0o700 != 0o700 {
        // SAFETY: open fd.
        if unsafe { libc::fchmod(fd.0, (st.st_mode & 0o7777) | 0o700) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(Doomed { left: list(&fd)?, fd, name, dev })
}

/// The names in the open folder `fd`, `.` and `..` left out, each with
/// whether it is a folder where `d_type` tells.
fn list(fd: &DirFd) -> io::Result<Vec<(CString, Option<bool>)>> {
    // `fdopendir` takes the descriptor it is given: a duplicate, closed with it.
    // SAFETY: open fd.
    let dup = unsafe { libc::dup(fd.0) };
    if dup < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `dup` is an open descriptor this call now owns.
    let dir = unsafe { libc::fdopendir(dup) };
    if dir.is_null() {
        let e = io::Error::last_os_error();
        // SAFETY: `fdopendir` failed, so `dup` is still ours to close.
        unsafe { libc::close(dup) };
        return Err(e);
    }
    let mut out = Vec::new();
    let result = loop {
        // SAFETY: errno is this thread's; `readdir` sets it only on an error.
        unsafe { *libc::__error() = 0 };
        // SAFETY: `dir` is an open stream.
        let entry = unsafe { libc::readdir(dir) };
        if entry.is_null() {
            // SAFETY: as above.
            let errno = unsafe { *libc::__error() };
            break if errno == 0 { Ok(()) } else { Err(io::Error::from_raw_os_error(errno)) };
        }
        // SAFETY: `readdir` returned an entry, valid until the next call.
        let (name, kind) = unsafe { (std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()), (*entry).d_type) };
        if name.to_bytes() != b"." && name.to_bytes() != b".." {
            let is_dir = match kind {
                libc::DT_UNKNOWN => None,
                kind => Some(kind == libc::DT_DIR),
            };
            out.push((name.to_owned(), is_dir));
        }
    };
    // SAFETY: `dir` is open and closed once, with its descriptor.
    unsafe { libc::closedir(dir) };
    result.map(|()| out)
}
