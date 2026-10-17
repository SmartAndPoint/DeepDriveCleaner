//! Volume statistics (`statfs(2)`).

use std::ffi::{CStr, CString};
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeStats {
    /// `apfs`, `hfs`, `smbfs`, …
    pub fs_type: String,
    pub mount_point: PathBuf,
    /// Bytes available to an unprivileged user.
    pub available_bytes: u64,
    pub total_bytes: u64,
}

pub fn statfs(path: &Path) -> io::Result<VolumeStats> {
    let c_path = CString::new(path.as_os_str().as_bytes())?;
    // SAFETY: an all-zero `statfs` is a valid value; the call fills it.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c_path` is NUL-terminated and `st` is a valid out-pointer.
    if unsafe { libc::statfs(c_path.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the kernel NUL-terminates both fixed-size name buffers.
    let (fs_type, mount_point) = unsafe {
        (
            CStr::from_ptr(st.f_fstypename.as_ptr()).to_string_lossy().into_owned(),
            PathBuf::from(std::ffi::OsStr::from_bytes(CStr::from_ptr(st.f_mntonname.as_ptr()).to_bytes())),
        )
    };
    let block = u64::from(st.f_bsize);
    Ok(VolumeStats {
        fs_type,
        mount_point,
        available_bytes: st.f_bavail.saturating_mul(block),
        total_bytes: st.f_blocks.saturating_mul(block),
    })
}
