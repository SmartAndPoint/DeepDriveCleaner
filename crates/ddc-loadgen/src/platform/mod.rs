//! The load generator's OS-specific parts (ADR "Platform seam: one platform
//! module per OS with declared capabilities", §2, amended): the `*at`
//! builder, path names from bytes, a death by signal, and the owner and mode
//! checks of a target folder. No file of the generator outside this folder
//! names an OS, `std::os`, `libc` or `rustix`.

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as imp;
#[cfg(windows)]
use windows as imp;

pub use imp::{TEMP_MAX, build, builds, exit_signal, is_mine, name, owner_has_rwx, unlock_dir};

// Every OS module provides these with these signatures.
mod pins {
    use std::ffi::OsString;
    use std::fs::Metadata;
    use std::io;
    use std::path::Path;
    use std::process::ExitStatus;

    use super::imp;
    use crate::desc::Desc;

    const _: Option<u64> = imp::TEMP_MAX;
    const _: fn() -> io::Result<()> = imp::builds;
    const _: fn(&Desc, &Path) -> io::Result<()> = imp::build;
    const _: fn(&[u8]) -> Option<OsString> = imp::name;
    const _: fn(&ExitStatus) -> Option<i32> = imp::exit_signal;
    const _: fn(&Metadata) -> bool = imp::is_mine;
    const _: fn(&Metadata) -> bool = imp::owner_has_rwx;
    const _: fn(&Path) -> io::Result<()> = imp::unlock_dir;
}
