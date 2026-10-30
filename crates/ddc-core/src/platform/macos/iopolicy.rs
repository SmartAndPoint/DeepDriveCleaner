//! Dataless-file policy: with materialization off, touching an evicted
//! iCloud / File Provider item fails with `EDEADLK` instead of downloading it.

use std::io;

use libc::c_int;

// <sys/resource.h>
const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: c_int = 3;
const IOPOL_SCOPE_PROCESS: c_int = 0;
const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: c_int = 1;

unsafe extern "C" {
    fn setiopolicy_np(iotype: c_int, scope: c_int, policy: c_int) -> c_int;
    fn getiopolicy_np(iotype: c_int, scope: c_int) -> c_int;
}

/// Turns dataless materialization off for the whole process.
pub fn disable_dataless_materialization() -> io::Result<()> {
    // SAFETY: integer-only syscall wrapper.
    let rc = unsafe {
        setiopolicy_np(
            IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
            IOPOL_SCOPE_PROCESS,
            IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
        )
    };
    if rc == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// Whether materialization is currently off for this process.
pub fn dataless_materialization_disabled() -> io::Result<bool> {
    // SAFETY: integer-only syscall wrapper.
    let rc = unsafe { getiopolicy_np(IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES, IOPOL_SCOPE_PROCESS) };
    if rc < 0 { Err(io::Error::last_os_error()) } else { Ok(rc == IOPOL_MATERIALIZE_DATALESS_FILES_OFF) }
}
