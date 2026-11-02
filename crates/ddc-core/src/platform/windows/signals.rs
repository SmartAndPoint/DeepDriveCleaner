//! Ctrl-C, Ctrl-Break and a closed console during a clean, on Windows
//! (story "Windows scanner and rule catalog", step 9c-1; safety ADR, "§5
//! and §6, the command runner on Windows").
//!
//! A console control handler (`SetConsoleCtrlHandler`) stores the event as
//! the signal Unix would have sent (Ctrl-C and Ctrl-Break are 2, closing the
//! console is 1), sets the cancel flag the executor checks between items,
//! and ends every published job: the running command is ended, not asked,
//! as the ADR says. ddc then ends with the shell's code for that signal
//! ([`reraise`]: 130 or 129). Logoff and shutdown never reach a console
//! program. Closing the console leaves ddc 5 seconds: Windows ends the
//! process when the handler returns or the time runs out, so the handler
//! waits, and the report may be lost.
//!
//! The jobs are kept under a lock, not in atomics as on Unix: the handler
//! runs on a thread of its own, where a lock is allowed, and a handle value
//! is reused once closed, so a reader without the lock could end another
//! command's job.

use std::io::{self, Write as _};
use std::os::windows::io::{AsRawHandle as _, OwnedHandle};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, SetConsoleCtrlHandler};
use windows_sys::Win32::System::JobObjects::TerminateJobObject;
use windows_sys::core::BOOL;

/// Ends a command at once (on Windows every stop does).
pub const SIGKILL: i32 = 9;
/// A closed console, as a closed terminal's hangup on Unix.
pub(super) const SIGHUP: i32 = 1;
/// Ctrl-C and Ctrl-Break.
pub(super) const SIGINT: i32 = 2;

/// The exit code of a command whose job ddc ended.
const ENDED: u32 = 1;
/// How long the handler holds a closing console open for the report:
/// under the 5 seconds Windows gives.
const CLOSE_GRACE: Duration = Duration::from_millis(4_500);
/// Said at once: the current item, a long delete included, runs to its end.
const NOTICE: &[u8] = b"\nddc: stopping after the current item\n";

static CANCEL: AtomicBool = AtomicBool::new(false);
static SIGNAL: AtomicI32 = AtomicI32::new(0);
/// The jobs of the commands running now, by their pid.
static JOBS: Mutex<Vec<(i32, OwnedHandle)>> = Mutex::new(Vec::new());

fn jobs() -> MutexGuard<'static, Vec<(i32, OwnedHandle)>> {
    JOBS.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn end(job: &OwnedHandle) {
    // SAFETY: the job handle is open for the call.
    unsafe { TerminateJobObject(job.as_raw_handle(), ENDED) };
}

/// Publishes the job of the command `pid`, just started suspended: true to
/// resume it; false when a cancel came first, and the job is ended.
pub(super) fn publish(pid: u32, job: OwnedHandle) -> bool {
    let mut jobs = jobs();
    if received().is_some() {
        end(&job);
        return false;
    }
    jobs.push((i32::try_from(pid).unwrap_or(0), job));
    true
}

/// Stores `sig`, sets the cancel flag and ends every published job, as the
/// console handler does.
pub(super) fn cancel(sig: i32) {
    SIGNAL.store(sig, Ordering::SeqCst);
    CANCEL.store(true, Ordering::SeqCst);
    for (_, job) in jobs().iter() {
        end(job);
    }
}

/// The signal a console control event stands for; `None` for one ddc
/// leaves to Windows (logoff and shutdown reach only services).
pub(super) fn signal_of(kind: u32) -> Option<i32> {
    match kind {
        CTRL_C_EVENT | CTRL_BREAK_EVENT => Some(SIGINT),
        CTRL_CLOSE_EVENT => Some(SIGHUP),
        _ => None,
    }
}

/// The console control handler.
unsafe extern "system" fn on_ctrl(kind: u32) -> BOOL {
    let Some(sig) = signal_of(kind) else { return 0 };
    cancel(sig);
    // A write to a closed console fails, which is fine.
    let _ = io::stderr().write_all(NOTICE);
    if kind == CTRL_CLOSE_EVENT {
        // Windows ends the process when this returns: the main thread gets
        // the time to write the journal and the report, and ends it sooner.
        std::thread::sleep(CLOSE_GRACE);
    }
    1
}

/// Installs the console control handler and returns the flag it sets.
pub fn install() -> io::Result<&'static AtomicBool> {
    static INSTALLED: OnceLock<Result<(), String>> = OnceLock::new();
    INSTALLED
        .get_or_init(|| {
            // SAFETY: a handler of the documented signature, added once for
            // the life of the process.
            if unsafe { SetConsoleCtrlHandler(Some(on_ctrl), 1) } == 0 {
                Err(io::Error::last_os_error().to_string())
            } else {
                Ok(())
            }
        })
        .as_ref()
        .map(|()| &CANCEL)
        .map_err(|e| io::Error::other(e.clone()))
}

/// The signal the handler received, if any.
pub fn received() -> Option<i32> {
    match SIGNAL.load(Ordering::SeqCst) {
        0 => None,
        sig => Some(sig),
    }
}

/// The command `group` (its pid) was published when it started; if a
/// signal came meanwhile, its job is ended now, so it is never lost.
pub fn set_child_group(group: i32) {
    if received().is_some() {
        kill_group(group, SIGKILL);
    }
}

/// Forgets the job of `group`, whose command has ended, and closes it
/// without ending what the command left.
pub fn clear_child_group(group: i32) {
    jobs().retain(|(pid, _)| *pid != group);
}

/// The groups published now, for a test.
#[cfg(test)]
pub(crate) fn child_groups() -> Vec<i32> {
    jobs().iter().map(|(pid, _)| *pid).collect()
}

/// The limit flags of the job of `group`, for a test.
#[cfg(test)]
pub(super) fn limit_flags(group: i32) -> Option<u32> {
    use windows_sys::Win32::System::JobObjects::{
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, QueryInformationJobObject,
    };
    let jobs = jobs();
    let (_, job) = jobs.iter().find(|(pid, _)| *pid == group)?;
    // SAFETY: plain data; all zeroes is a valid value.
    let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    let size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).unwrap();
    // SAFETY: the job is open; `info` is the class's struct, `size` bytes.
    let ok = unsafe {
        QueryInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&raw mut info).cast(),
            size,
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(info.BasicLimitInformation.LimitFlags)
}

/// How many processes the job of `group` holds, for a test.
#[cfg(test)]
pub(super) fn active_processes(group: i32) -> u32 {
    use windows_sys::Win32::System::JobObjects::{
        JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JobObjectBasicAccountingInformation, QueryInformationJobObject,
    };
    let jobs = jobs();
    let Some((_, job)) = jobs.iter().find(|(pid, _)| *pid == group) else { return 0 };
    // SAFETY: plain data; all zeroes is a valid value.
    let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = unsafe { std::mem::zeroed() };
    let size = u32::try_from(size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>()).unwrap();
    // SAFETY: the job is open; `info` is the class's struct, `size` bytes.
    let ok = unsafe {
        QueryInformationJobObject(
            job.as_raw_handle(),
            JobObjectBasicAccountingInformation,
            (&raw mut info).cast(),
            size,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 { 0 } else { info.ActiveProcesses }
}

/// Ends the job of `group`: the command and everything it started. Nothing
/// when no such command runs.
pub fn kill_group(group: i32, _sig: i32) {
    let _ = signal_group(group, SIGKILL);
}

/// Ends the job of `group`; an error when no such command runs.
pub fn signal_group(group: i32, _sig: i32) -> io::Result<()> {
    let jobs = jobs();
    let (_, job) = jobs
        .iter()
        .find(|(pid, _)| *pid == group)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("no command runs as {group}")))?;
    end(job);
    Ok(())
}

/// Ends the process with the shell's code for a death by `sig`.
pub fn reraise(sig: i32) -> ! {
    std::process::exit(128 + sig)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each console event stands for the signal Unix would send; logoff and
    /// shutdown are left to Windows.
    #[test]
    fn console_events_are_the_signals_unix_would_send() {
        assert_eq!(signal_of(CTRL_C_EVENT), Some(SIGINT));
        assert_eq!(signal_of(CTRL_BREAK_EVENT), Some(SIGINT));
        assert_eq!(signal_of(CTRL_CLOSE_EVENT), Some(SIGHUP));
        assert_eq!(signal_of(5), None, "logoff");
        assert_eq!(signal_of(6), None, "shutdown");
    }

    /// Ctrl-C sets the flag and the signal, as SIGINT does on Unix.
    #[test]
    fn ctrl_c_is_signal_2_and_sets_the_flag() {
        let flag = install().unwrap();
        // SAFETY: the handler called as Windows would, on this thread.
        assert_eq!(unsafe { on_ctrl(CTRL_C_EVENT) }, 1);
        assert!(flag.load(Ordering::SeqCst));
        assert_eq!(received(), Some(SIGINT));
    }

    #[test]
    fn a_group_nobody_published_is_not_found() {
        assert_eq!(signal_group(4242, SIGKILL).unwrap_err().kind(), io::ErrorKind::NotFound);
        kill_group(4242, SIGKILL);
    }
}
