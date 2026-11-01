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
}

#[cfg(not(target_os = "linux"))] // ported: `linux/fsops.rs`
pub mod fsops {
    use std::convert::Infallible;
    use std::ffi::OsStr;
    use std::fs::File;
    use std::io;
    use std::path::Path;
    use std::time::SystemTime;

    use crate::platform::types::EntryStat;

    pub fn set_times(_path: &Path, _modified: SystemTime, _added: SystemTime) -> io::Result<()> {
        super::unsupported()
    }

    pub fn open_nofollow_nonblock(_path: &Path) -> io::Result<File> {
        super::unsupported()
    }

    pub fn can_write(_path: &Path) -> bool {
        false
    }

    /// Never made: [`DirFd::open`] fails, so no verified-parent operation runs.
    #[derive(Debug)]
    pub struct DirFd(Infallible);

    impl DirFd {
        pub fn open(_path: &Path) -> io::Result<Self> {
            super::unsupported()
        }

        pub fn identity(&self) -> io::Result<(u64, u64)> {
            match self.0 {}
        }

        pub fn stat_child(&self, _name: &OsStr) -> io::Result<EntryStat> {
            match self.0 {}
        }

        pub fn child_times(&self, _name: &OsStr) -> io::Result<(Option<i64>, Option<i64>)> {
            match self.0 {}
        }

        pub fn rename_child(&self, _from: &OsStr, _to: &OsStr) -> io::Result<()> {
            match self.0 {}
        }

        pub fn unlink_child(&self, _name: &OsStr) -> io::Result<()> {
            match self.0 {}
        }
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

#[cfg(not(target_os = "linux"))] // ported: `linux/meta.rs`
pub mod meta {
    use std::fs::{Metadata, Permissions};
    use std::io;

    /// Fails closed: no identity is made up.
    pub fn file_id(_md: &Metadata) -> io::Result<(u64, u64)> {
        super::unsupported()
    }

    pub fn link_count(_md: &Metadata) -> u64 {
        1
    }

    pub fn alloc_bytes(_md: &Metadata) -> u64 {
        0
    }

    pub fn mtime(_md: &Metadata) -> i64 {
        0
    }

    pub fn owner(_md: &Metadata) -> Option<u32> {
        None
    }

    /// The agents report's read gate (see [`owner`] for the removal
    /// guard's): no owner is made up, so a folder that must be the user's
    /// is not.
    pub fn uid(_md: &Metadata) -> Option<u32> {
        None
    }

    pub fn st_flags(_md: &Metadata) -> u32 {
        0
    }

    pub fn add_owner_rwx(_perms: &mut Permissions) {}

    /// No tool is found.
    pub fn is_executable(_md: &Metadata) -> bool {
        false
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
