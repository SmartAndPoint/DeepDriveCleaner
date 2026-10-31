//! Directory reader on `getattrlistbulk(2)`: one syscall returns a batch of
//! entries with name, type, inode, device, flags, link count, allocated size
//! and, on request, the APFS private size and the modification and added
//! times ([`Times`]). Only the syscalls are `unsafe`; the attribute buffer is
//! parsed with bounds-checked safe code.

use std::cell::RefCell;
use std::ffi::{CString, OsStr};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use libc::c_int;

use crate::platform::types::{DirListing, EntryKind, EntryMeta, ReadError, ReadErrorKind, Times};

// <sys/attr.h>
pub(super) const ATTR_BIT_MAP_COUNT: u16 = 5;
const ATTR_CMN_NAME: u32 = 0x0000_0001;
const ATTR_CMN_DEVID: u32 = 0x0000_0002;
const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
pub(super) const ATTR_CMN_MODTIME: u32 = 0x0000_0400;
const ATTR_CMN_FLAGS: u32 = 0x0004_0000;
const ATTR_CMN_FILEID: u32 = 0x0200_0000;
pub(super) const ATTR_CMN_ADDEDTIME: u32 = 0x1000_0000;
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
pub(super) const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
const ATTR_FILE_LINKCOUNT: u32 = 0x0000_0001;
const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;
const ATTR_CMNEXT_PRIVATESIZE: u32 = 0x0000_0008;
const FSOPT_ATTR_CMN_EXTENDED: u64 = 0x0000_0020;

// <sys/vnode.h> enum vtype
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;

const BUF_LEN: usize = 128 * 1024;

#[repr(C)]
pub(super) struct AttrList {
    pub bitmapcount: u16,
    pub reserved: u16,
    pub commonattr: u32,
    pub volattr: u32,
    pub dirattr: u32,
    pub fileattr: u32,
    pub forkattr: u32,
}

unsafe extern "C" {
    fn getattrlistbulk(
        dirfd: c_int,
        attr_list: *mut AttrList,
        attr_buf: *mut libc::c_void,
        attr_buf_size: libc::size_t,
        options: u64,
    ) -> c_int;
}

thread_local! {
    // One attribute buffer per worker thread; rayon threads live for the whole walk.
    static BUF: RefCell<Vec<u8>> = RefCell::new(vec![0; BUF_LEN]);
}

/// Owned file descriptor, closed on drop.
struct Fd(c_int);

impl Drop for Fd {
    fn drop(&mut self) {
        // SAFETY: we own the descriptor and close it exactly once.
        unsafe { libc::close(self.0) };
    }
}

/// Lists `dir` by `getattrlistbulk` (macOS, APFS/HFS+), as
/// `scan::reader::SystemReader` asks: without following a symlink, nothing
/// read when the directory is not `expect`, the private size with
/// `want_private`, the entries' `times`. The directory's own modification
/// time comes from the `fstat` that checks `expect`.
pub fn list(dir: &Path, expect: Option<(u64, u64)>, want_private: bool, times: Times) -> Result<DirListing, ReadError> {
    let path = CString::new(dir.as_os_str().as_bytes()).map_err(|_| ReadError::from_errno(libc::EINVAL))?;
    // Retried on EINTR, as std does: a signal is not an unreadable directory.
    let raw = loop {
        // SAFETY: `path` is a valid NUL-terminated string.
        let raw = unsafe {
            libc::open(path.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        };
        if raw >= 0 || super::last_errno() != libc::EINTR {
            break raw;
        }
    };
    if raw < 0 {
        return Err(ReadError::from_errno(super::last_errno()));
    }
    let fd = Fd(raw);

    // SAFETY: an all-zero `stat` is a valid value; fstat fills it.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `fd` is open and `st` is a valid out-pointer.
    if unsafe { libc::fstat(fd.0, &mut st) } != 0 {
        return Err(ReadError::from_errno(super::last_errno()));
    }
    // Same widening as std's `MetadataExt::dev()`.
    let (dev, ino, modified) = (st.st_dev as u64, st.st_ino, st.st_mtime);
    if let Some((want_dev, want_ino)) = expect {
        if dev != want_dev {
            return Err(ReadError::new(ReadErrorKind::OtherFilesystem, None));
        }
        if ino != want_ino {
            return Err(ReadError::new(ReadErrorKind::Changed, None));
        }
    }

    let mut attrs = AttrList {
        bitmapcount: ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: ATTR_CMN_RETURNED_ATTRS
            | ATTR_CMN_NAME
            | ATTR_CMN_ERROR
            | ATTR_CMN_DEVID
            | ATTR_CMN_OBJTYPE
            | ATTR_CMN_FLAGS
            | ATTR_CMN_FILEID
            | match times {
                Times::None => 0,
                Times::Modified => ATTR_CMN_MODTIME,
                Times::ModifiedAndAdded => ATTR_CMN_MODTIME | ATTR_CMN_ADDEDTIME,
            },
        volattr: 0,
        dirattr: 0,
        fileattr: ATTR_FILE_LINKCOUNT | ATTR_FILE_ALLOCSIZE,
        forkattr: if want_private { ATTR_CMNEXT_PRIVATESIZE } else { 0 },
    };
    let options = if want_private { FSOPT_ATTR_CMN_EXTENDED } else { 0 };

    let mut entries = Vec::new();
    BUF.with(|cell| {
        let mut buf = cell.borrow_mut();
        loop {
            // SAFETY: `fd` is an open directory, `attrs` is a valid attrlist and
            // `buf` is writable for `buf.len()` bytes.
            let count = unsafe { getattrlistbulk(fd.0, &mut attrs, buf.as_mut_ptr().cast(), buf.len(), options) };
            match count {
                0 => return Ok(()),
                n if n < 0 => match super::last_errno() {
                    // Interrupted before returning a batch: nothing was consumed, call again.
                    libc::EINTR => continue,
                    errno => return Err(ReadError::from_errno(errno)),
                },
                n => parse_batch(&buf, n as usize, want_private, &mut entries)?,
            }
        }
    })?;
    // Counted once the listing is whole; a listing that fails part-way counts nothing.
    crate::stats::add(crate::stats::Counter::DirEntriesRead, entries.len() as u64);
    Ok(DirListing { dev, ino, modified: Some(modified), alloc: st.st_blocks as u64 * 512, entries })
}

/// Bounds-checked reader over one attribute record.
struct Cursor<'a> {
    rec: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], ReadError> {
        let bytes = self.rec.get(self.pos..self.pos + N).ok_or_else(corrupt)?;
        self.pos += N;
        Ok(bytes.try_into().expect("slice length is N"))
    }
    fn u32(&mut self) -> Result<u32, ReadError> {
        self.take::<4>().map(u32::from_ne_bytes)
    }
    fn i32(&mut self) -> Result<i32, ReadError> {
        self.take::<4>().map(i32::from_ne_bytes)
    }
    fn u64(&mut self) -> Result<u64, ReadError> {
        self.take::<8>().map(u64::from_ne_bytes)
    }
    fn i64(&mut self) -> Result<i64, ReadError> {
        self.take::<8>().map(i64::from_ne_bytes)
    }
    /// A `struct timespec` (16 bytes): its seconds.
    fn seconds(&mut self) -> Result<i64, ReadError> {
        let secs = self.i64()?;
        let _nanos = self.i64()?;
        Ok(secs)
    }
}

fn corrupt() -> ReadError {
    ReadError::new(ReadErrorKind::Other, Some(libc::EBADMSG))
}

/// Parses `count` records. Layout per record (see getattrlistbulk(2)): `u32`
/// length, `attribute_set_t` of returned attributes, `ATTR_CMN_ERROR` if
/// returned, then the remaining returned attributes in bit order (common →
/// file → fork/cmnext), each 4-byte aligned.
fn parse_batch(buf: &[u8], count: usize, want_private: bool, out: &mut Vec<EntryMeta>) -> Result<(), ReadError> {
    let mut start = 0usize;
    for _ in 0..count {
        let len = buf
            .get(start..start + 4)
            .map(|b| u32::from_ne_bytes(b.try_into().expect("4 bytes")) as usize)
            .ok_or_else(corrupt)?;
        let rec = buf.get(start..start + len).filter(|_| len >= 4).ok_or_else(corrupt)?;
        let mut c = Cursor { rec, pos: 4 };

        let ret_common = c.u32()?;
        let _ret_vol = c.u32()?;
        let _ret_dir = c.u32()?;
        let ret_file = c.u32()?;
        let ret_fork = c.u32()?;

        let error = if ret_common & ATTR_CMN_ERROR != 0 { c.u32()? as i32 } else { 0 };
        let name: &[u8] = if ret_common & ATTR_CMN_NAME != 0 {
            let name_ref_at = c.pos;
            let name_off = c.i32()?;
            let name_len = c.u32()? as usize;
            let name_at = name_ref_at.checked_add_signed(name_off as isize).ok_or_else(corrupt)?;
            let raw_name = rec.get(name_at..name_at + name_len).ok_or_else(corrupt)?;
            raw_name.split(|b| *b == 0).next().unwrap_or_default()
        } else if error != 0 {
            b"" // an error entry may come without a name
        } else {
            return Err(corrupt());
        };

        let dev = if ret_common & ATTR_CMN_DEVID != 0 { c.i32()? as u64 } else { 0 };
        let objtype = if ret_common & ATTR_CMN_OBJTYPE != 0 { c.u32()? } else { 0 };
        let modified = if ret_common & ATTR_CMN_MODTIME != 0 { Some(c.seconds()?) } else { None };
        let flags = if ret_common & ATTR_CMN_FLAGS != 0 { c.u32()? } else { 0 };
        let ino = if ret_common & ATTR_CMN_FILEID != 0 { c.u64()? } else { 0 };
        let added = if ret_common & ATTR_CMN_ADDEDTIME != 0 { Some(c.seconds()?) } else { None };
        let nlink = if ret_file & ATTR_FILE_LINKCOUNT != 0 { c.u32()? } else { 1 };
        let alloc = if ret_file & ATTR_FILE_ALLOCSIZE != 0 { c.i64()?.max(0) as u64 } else { 0 };
        let private =
            if want_private && ret_fork & ATTR_CMNEXT_PRIVATESIZE != 0 { Some(c.i64()?.max(0) as u64) } else { None };

        out.push(EntryMeta {
            name: OsStr::from_bytes(name).to_os_string(),
            kind: match objtype {
                VREG => EntryKind::File,
                VDIR => EntryKind::Dir,
                VLNK => EntryKind::Symlink,
                _ => EntryKind::Other,
            },
            dev,
            ino,
            nlink,
            uid: None,
            flags,
            alloc,
            private,
            error: (error != 0).then_some(error),
            modified,
            added,
        });
        start += len;
    }
    Ok(())
}
