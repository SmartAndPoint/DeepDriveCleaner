//! Open-file limit. Windows has no per-process limit on open handles that
//! a walk could reach, so there is nothing to raise (seam ADR §1: the item
//! exists on every OS).

use std::io;

/// No soft limit: `u64::MAX`.
pub fn nofile_limit() -> io::Result<u64> {
    Ok(u64::MAX)
}

/// Nothing to raise: the limit as it is.
pub fn raise_nofile_limit() -> io::Result<u64> {
    nofile_limit()
}
