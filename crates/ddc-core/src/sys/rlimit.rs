//! Open-file limit. launchd starts agents with a soft limit of 256.

use std::io;

/// macOS rejects `RLIM_INFINITY` for `RLIMIT_NOFILE`; `OPEN_MAX` is the ceiling.
const OPEN_MAX: libc::rlim_t = 10240;

/// Current soft `RLIMIT_NOFILE`.
pub fn nofile_limit() -> io::Result<u64> {
    let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: `lim` is a valid out-pointer.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(lim.rlim_cur)
}

/// Raises the soft `RLIMIT_NOFILE` to `min(hard, OPEN_MAX)`; returns the new soft limit.
pub fn raise_nofile_limit() -> io::Result<u64> {
    let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
    // SAFETY: `lim` is a valid out-pointer.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let target = lim.rlim_max.min(OPEN_MAX);
    if lim.rlim_cur < target {
        lim.rlim_cur = target;
        // SAFETY: `lim` is a valid, initialized rlimit.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &lim) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(lim.rlim_cur)
}
