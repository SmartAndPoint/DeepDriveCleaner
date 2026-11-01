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
//! - a plain value is neutral (`None`, empty, `false`, 0);
//! - the in-use checks (`procs`) are empty, which fails open. They are never
//!   reached: `require_supported` refuses in `execute`, `preflight` and
//!   `free::run`, and the CLI refuses at dispatch before them. A port fills
//!   them before it sets `SUPPORTED`.
//!
//! Nothing here reads a directory or the environment. A module an OS has
//! ported is compiled out for it, so its own module is the only one.

use std::io;

fn unsupported<T>() -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "not available on this platform yet"))
}

#[cfg(not(target_os = "linux"))] // ported: `linux/bulk.rs`
pub mod bulk {
    use std::path::Path;

    use crate::platform::types::{DirListing, ReadError, ReadErrorKind, Times};

    pub fn list(
        _dir: &Path,
        _expect: Option<(u64, u64)>,
        _private: bool,
        _times: Times,
    ) -> Result<DirListing, ReadError> {
        Err(ReadError::new(ReadErrorKind::Other, None))
    }

    #[derive(Debug)]
    pub struct Anchor;

    pub fn anchor(_root: &Path) -> std::io::Result<Anchor> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    pub fn list_in(
        _anchor: &Anchor,
        _dir: &Path,
        _expect: Option<(u64, u64)>,
        _private: bool,
        _times: Times,
    ) -> Result<DirListing, ReadError> {
        Err(ReadError::new(ReadErrorKind::Other, None))
    }
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

#[cfg(not(target_os = "linux"))] // ported: `linux/process.rs`
pub mod process {
    use std::process::Command;

    pub fn own_process_group(cmd: &mut Command) -> &mut Command {
        cmd
    }
}

#[cfg(not(target_os = "linux"))] // ported: `linux/procs.rs`
pub mod procs {
    //! Empty, which fails open; unreachable (see the module doc).
    use std::path::PathBuf;

    pub use crate::platform::types::{OpenVnode, ProcInfo, ProcStart, app_bundle};

    /// No user's id: never compared, since nothing here owns a process.
    pub fn uid() -> u32 {
        u32::MAX
    }

    pub fn real_home() -> Option<PathBuf> {
        None
    }

    pub fn is_root() -> bool {
        false
    }

    pub fn current_user_processes() -> Vec<ProcInfo> {
        Vec::new()
    }

    pub fn current_user_processes_unopened() -> Vec<ProcInfo> {
        Vec::new()
    }

    pub fn open_files(_pid: i32) -> Vec<PathBuf> {
        Vec::new()
    }

    pub fn open_vnodes(_pid: i32) -> Vec<OpenVnode> {
        Vec::new()
    }

    pub fn process_start(_pid: i32) -> Option<ProcStart> {
        None
    }

    pub fn thread_count(_pid: i32) -> Option<u32> {
        None
    }
}

#[cfg(not(target_os = "linux"))] // ported: `linux/signals.rs`
pub mod signals {
    use std::io;
    use std::sync::atomic::AtomicBool;

    pub const SIGKILL: i32 = 9;

    pub fn install() -> io::Result<&'static AtomicBool> {
        super::unsupported()
    }

    pub fn received() -> Option<i32> {
        None
    }

    pub fn set_child_group(_group: i32) {}

    pub fn clear_child_group() {}

    pub fn kill_group(_group: i32, _sig: i32) {}

    pub fn signal_group(_group: i32, _sig: i32) -> io::Result<()> {
        super::unsupported()
    }

    /// Ends the process with the shell's code for a death by `sig`.
    pub fn reraise(sig: i32) -> ! {
        std::process::exit(128 + sig)
    }
}

#[cfg(not(target_os = "linux"))] // ported: `linux/statfs.rs`
pub mod statfs {
    use std::io;
    use std::path::Path;

    pub use crate::platform::types::VolumeStats;

    pub fn statfs(_path: &Path) -> io::Result<VolumeStats> {
        super::unsupported()
    }

    pub fn mounts() -> io::Result<Vec<VolumeStats>> {
        super::unsupported()
    }
}

#[cfg(not(target_os = "linux"))] // ported: `linux/trash.rs`
pub mod trash {
    use std::io;
    use std::path::Path;

    pub fn move_to_trash(_path: &Path) -> io::Result<()> {
        super::unsupported()
    }
}

#[cfg(not(target_os = "linux"))] // ported: `linux/usage.rs`
pub mod usage {
    use std::io;
    use std::process::ExitStatus;
    use std::time::Duration;

    pub use crate::platform::types::Usage;

    pub fn wait4(_pid: u32, _nohang: bool) -> io::Result<Option<(ExitStatus, Usage)>> {
        super::unsupported()
    }

    pub fn self_usage() -> io::Result<Usage> {
        super::unsupported()
    }

    pub fn thread_cpu_time() -> io::Result<Duration> {
        super::unsupported()
    }

    pub fn load_average() -> Option<f64> {
        None
    }

    pub fn machine_model() -> Option<String> {
        None
    }
}
