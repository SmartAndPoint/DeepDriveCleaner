//! The CLI's OS-specific parts (ADR "Platform seam: one platform module per
//! OS with declared capabilities", §2, amended): the process setup, the
//! notification the CLI shows itself, and the scheduled scan. No file of the
//! CLI outside this folder names an OS, `std::os` or `libc`.
//!
//! The schedulers are shared, not per OS: only `schedule` reaches them, through
//! [`scheduler()`], and the dispatcher refuses that without `Capability::Schedule`.

pub mod launchd;
#[cfg(target_os = "macos")]
mod macos;
pub mod scheduler;
pub mod systemd;
#[cfg(not(target_os = "macos"))]
mod unported;

#[cfg(target_os = "macos")]
use macos as imp;
#[cfg(not(target_os = "macos"))]
use unported as imp;

pub use imp::SystemNotifier;

/// The scheduler of this user: the systemd user timer on Linux, the
/// LaunchAgent elsewhere (Windows declares no `schedule`, so the dispatcher
/// refuses it there; DDC-008's Task Scheduler takes that arm). Chosen at run
/// time, so both backends are built and used on every target. Built lazily:
/// nothing here asks the system anything.
pub fn scheduler(env: &ddc_core::env::Env) -> Box<dyn scheduler::Scheduler> {
    static LAUNCHCTL: launchd::SystemLaunchctl = launchd::SystemLaunchctl;
    static SYSTEMCTL: systemd::SystemSystemctl = systemd::SystemSystemctl;
    match ddc_core::platform::PLATFORM {
        ddc_core::platform::Platform::Linux => {
            Box::new(systemd::Systemd { home: env.home().to_path_buf(), sc: &SYSTEMCTL })
        }
        _ => Box::new(launchd::Launchd {
            home: env.home().to_path_buf(),
            uid: ddc_core::platform::procs::uid(),
            lc: &LAUNCHCTL,
        }),
    }
}

/// Process-wide setup that must happen before any filesystem access.
pub fn init_process() {
    // An elevated Windows token's backup, restore and take-ownership
    // privileges pass every ACL: ddc removes them before anything runs. If
    // that fails, the owner check refuses every scan and clean.
    if let Err(err) = ddc_core::platform::procs::drop_acl_bypass_privileges() {
        eprintln!("ddc: warning: cannot remove the privileges that pass every ACL: {err}");
    }
    // Never download evicted iCloud / File Provider content while scanning.
    if let Err(err) = ddc_core::platform::iopolicy::disable_dataless_materialization() {
        eprintln!("ddc: warning: cannot disable dataless file materialization: {err}");
    }
    // launchd starts agents with 256 open files; the walker needs headroom.
    if let Err(err) = ddc_core::platform::rlimit::raise_nofile_limit() {
        eprintln!("ddc: warning: cannot raise the open-file limit: {err}");
    }
}
