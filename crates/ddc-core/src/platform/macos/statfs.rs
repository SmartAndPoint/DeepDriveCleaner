//! Volume statistics (`statfs(2)`).

use std::ffi::{CStr, CString};
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

pub use crate::platform::types::VolumeStats;

pub fn statfs(path: &Path) -> io::Result<VolumeStats> {
    let c_path = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: an all-zero `statfs` is a valid value; the call fills it.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is NUL-terminated and `st` is a valid out-pointer.
    if unsafe { libc::statfs(c_path.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(stats(&st))
}

/// Every mounted file system, as `mount` lists them.
pub fn mounts() -> io::Result<Vec<VolumeStats>> {
    // SAFETY: a null buffer asks only for the count.
    let n = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    if n < 0 {
        return Err(io::Error::last_os_error());
    }
    // Room for mounts that appear between the two calls.
    let cap = usize::try_from(n).unwrap_or(0) + 8;
    // SAFETY: an all-zero `statfs` is a valid value.
    let mut buf: Vec<libc::statfs> = vec![unsafe { std::mem::zeroed() }; cap];
    let bytes = libc::c_int::try_from(cap * std::mem::size_of::<libc::statfs>()).map_err(io::Error::other)?;
    // SAFETY: `buf` holds `cap` entries and `bytes` is its size.
    let got = unsafe { libc::getfsstat(buf.as_mut_ptr(), bytes, libc::MNT_NOWAIT) };
    if got < 0 {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(usize::try_from(got).unwrap_or(0));
    Ok(buf.iter().map(stats).collect())
}

fn stats(st: &libc::statfs) -> VolumeStats {
    // SAFETY: the kernel NUL-terminates the fixed-size name buffers.
    let (fs_type, mount_point, mounted_from) = unsafe {
        (
            CStr::from_ptr(st.f_fstypename.as_ptr()).to_string_lossy().into_owned(),
            PathBuf::from(std::ffi::OsStr::from_bytes(CStr::from_ptr(st.f_mntonname.as_ptr()).to_bytes())),
            CStr::from_ptr(st.f_mntfromname.as_ptr()).to_string_lossy().into_owned(),
        )
    };
    let block = u64::from(st.f_bsize);
    VolumeStats {
        fs_type,
        mount_point,
        available_bytes: st.f_bavail.saturating_mul(block),
        total_bytes: st.f_blocks.saturating_mul(block),
        free_bytes: st.f_bfree.saturating_mul(block),
        mounted_from,
    }
}
