//! The OS modules of a platform that is not ported yet (ADR "Platform seam:
//! one platform module per OS with declared capabilities", §1, §2): one body,
//! which `linux/` and `windows/` re-export beside their own literal
//! switches. Every item keeps the signature pinned in `platform/mod.rs`, so
//! shared code compiles here, and none of it does anything:
//!
//! - an item returning `io::Result` returns `ErrorKind::Unsupported`;
//! - the I/O policy setter `init_process` calls returns `Ok`, so `rules`,
//!   the one command an unsupported platform runs, stays quiet (the
//!   open-file limit, the other one, is ported on both OSes this serves);
//! - a plain value is neutral (`None`, empty, `false`, 0).
//!
//! Nothing here reads a directory or the environment. A module an OS has
//! ported is compiled out for it, so its own module is the only one.

use std::io;

fn unsupported<T>() -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "not available on this platform yet"))
}

pub mod iopolicy {
    use std::io;

    /// A setter `init_process` calls: `Ok`, so `rules` stays quiet.
    pub fn disable_dataless_materialization() -> io::Result<()> {
        Ok(())
    }

    pub fn dataless_materialization_disabled() -> io::Result<bool> {
        super::unsupported()
    }
}

#[cfg(not(target_os = "linux"))] // ported: `linux/trash.rs`
pub mod trash {
    use std::ffi::OsStr;
    use std::path::Path;

    use crate::platform::fsops::DirFd;
    use crate::platform::types::{TrashError, TrashPlace};

    pub fn trash_item(_: &DirFd, _: &OsStr, _: &Path, _: (u64, u64)) -> Result<Option<TrashPlace>, TrashError> {
        Err(TrashError::Other(super::unsupported::<()>().unwrap_err()))
    }

    pub fn would_trash(_: &DirFd, _: &OsStr, _: &Path) -> Result<(), TrashError> {
        Err(TrashError::Other(super::unsupported::<()>().unwrap_err()))
    }
}
