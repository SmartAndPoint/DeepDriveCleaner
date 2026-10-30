//! The CLI's OS-specific parts (ADR "Platform seam: one platform module per
//! OS with declared capabilities", §2, amended): the process setup, the
//! notification the CLI shows itself, and the LaunchAgent. No file of the
//! CLI outside this folder names an OS, `std::os` or `libc`.
//!
//! `launchd` is shared, not per OS: only `schedule` reaches it, and the
//! dispatcher refuses that without `Capability::Schedule`.

pub mod launchd;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod unported;

#[cfg(target_os = "macos")]
use macos as imp;
#[cfg(not(target_os = "macos"))]
use unported as imp;

pub use imp::SystemNotifier;

/// Process-wide setup that must happen before any filesystem access.
pub fn init_process() {
    // Never download evicted iCloud / File Provider content while scanning.
    if let Err(err) = ddc_core::platform::iopolicy::disable_dataless_materialization() {
        eprintln!("ddc: warning: cannot disable dataless file materialization: {err}");
    }
    // launchd starts agents with 256 open files; the walker needs headroom.
    if let Err(err) = ddc_core::platform::rlimit::raise_nofile_limit() {
        eprintln!("ddc: warning: cannot raise the open-file limit: {err}");
    }
}
