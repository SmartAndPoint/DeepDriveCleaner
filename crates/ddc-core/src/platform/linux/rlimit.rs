//! Open-file limit. Linux starts most sessions with a soft limit of 1024.

use std::io;

use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

/// The ceiling ddc raises the soft limit to, as on macOS.
const WANTED: u64 = 10240;

/// Current soft `RLIMIT_NOFILE`.
pub fn nofile_limit() -> io::Result<u64> {
    Ok(getrlimit(Resource::Nofile).current.unwrap_or(u64::MAX))
}

/// Raises the soft `RLIMIT_NOFILE` to `min(hard, 10240)`; returns the new soft limit.
pub fn raise_nofile_limit() -> io::Result<u64> {
    let lim = getrlimit(Resource::Nofile);
    let current = lim.current.unwrap_or(u64::MAX);
    let target = lim.maximum.unwrap_or(u64::MAX).min(WANTED);
    if current >= target {
        return Ok(current);
    }
    setrlimit(Resource::Nofile, Rlimit { current: Some(target), maximum: lim.maximum })?;
    Ok(target)
}
