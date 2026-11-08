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
//! The Windows app's Stop is a named event (`--cancel-event`, [`watch_event`]):
//! once it is set, a command that installed the handler stops as on SIGTERM
//! (15, exit 143); one that did not (a scan) ends at once with 143, as
//! SIGTERM's default does on Unix.
//!
//! The jobs are kept under a lock, not in atomics as on Unix: the handler
//! runs on a thread of its own, where a lock is allowed, and a handle value
//! is reused once closed, so a reader without the lock could end another
//! command's job.

use std::io::{self, Write as _};
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
use windows_sys::Win32::System::Console::{CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, SetConsoleCtrlHandler};
use windows_sys::Win32::System::JobObjects::TerminateJobObject;
use windows_sys::Win32::System::Threading::{INFINITE, OpenEventW, WaitForSingleObject};
use windows_sys::core::BOOL;

/// Ends a command at once (on Windows every stop does).
pub const SIGKILL: i32 = 9;
/// A closed console, as a closed terminal's hangup on Unix.
pub(super) const SIGHUP: i32 = 1;
/// Ctrl-C and Ctrl-Break.
pub(super) const SIGINT: i32 = 2;
/// The Windows app's Stop (`--cancel-event`), as SIGTERM on macOS.
pub(super) const SIGTERM: i32 = 15;

/// The exit code of a command whose job ddc ended.
const ENDED: u32 = 1;
/// How long the handler holds a closing console open for the report:
/// under the 5 seconds Windows gives.
const CLOSE_GRACE: Duration = Duration::from_millis(4_500);
/// Said at once: the current item, a long delete included, runs to its end.
const NOTICE: &[u8] = b"\nddc: stopping after the current item\n";

static CANCEL: AtomicBool = AtomicBool::new(false);
/// Set once [`install`] ran: a command that reads the cancel flag.
static INSTALLED: OnceLock<Result<(), String>> = OnceLock::new();
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

/// Watches the named event the Windows app creates for this run
/// (`--cancel-event`): once it is set, the stop is SIGTERM's (15, exit 143):
/// the cancel flag, every running job ended, the journal and the report
/// written. Opened for `SYNCHRONIZE` only. An event already set stops the
/// run before its first item. Any process of the session can open a named
/// event, so at worst it stops a clean early, which is safe.
pub fn watch_event(name: &str) -> io::Result<()> {
    let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
    // SAFETY: a NUL-terminated wide name; the handle is checked below.
    let raw = unsafe { OpenEventW(SYNCHRONIZE, 0, wide.as_ptr()) };
    if raw.is_null() {
        let e = io::Error::last_os_error();
        return Err(if e.kind() == io::ErrorKind::NotFound {
            io::Error::new(e.kind(), "no such event: the app creates it before it starts ddc")
        } else {
            e
        });
    }
    // SAFETY: a handle OpenEventW just returned, owned by nobody else.
    let event = unsafe { OwnedHandle::from_raw_handle(raw) };
    let stop = || {
        // A command that reads no cancel flag (a scan, or a clean before it
        // installed its handler) ends at once, as SIGTERM's default does on
        // Unix: never a run that goes on with its probes refused.
        // Its running probes' jobs are ended first, and none starts after.
        cancel(SIGTERM);
        if INSTALLED.get().is_none_or(Result::is_err) {
            reraise(SIGTERM);
        }
        let _ = io::stderr().write_all(NOTICE);
    };
    // SAFETY: the event is open for the call; 0 is no wait.
    if unsafe { WaitForSingleObject(event.as_raw_handle(), 0) } == WAIT_OBJECT_0 {
        stop();
        return Ok(());
    }
    std::thread::Builder::new().name("ddc-cancel-event".into()).spawn(move || {
        // SAFETY: the event is open for as long as this thread owns it.
        if unsafe { WaitForSingleObject(event.as_raw_handle(), INFINITE) } == WAIT_OBJECT_0 {
            stop();
        } else {
            let e = io::Error::last_os_error();
            let _ = writeln!(io::stderr(), "ddc: warning: the cancel event cannot be watched, Stop will not work: {e}");
        }
    })?;
    Ok(())
}

/// Not on Windows yet: `ddc lock stop` needs `in-use-check`.
pub fn interrupt(_pid: i32) -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "ddc lock stop is not ported to Windows"))
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

    /// An event with a name of this test's own, set or not.
    fn event(set: bool) -> (String, OwnedHandle) {
        use windows_sys::Win32::System::Threading::CreateEventW;
        let name = format!("ddc-test-cancel-{}-{set}", std::process::id());
        let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
        // SAFETY: no security attributes; manual reset; a NUL-terminated name.
        let raw = unsafe { CreateEventW(std::ptr::null(), 1, i32::from(set), wide.as_ptr()) };
        assert!(!raw.is_null(), "{}", io::Error::last_os_error());
        // SAFETY: a handle CreateEventW just returned, owned by nobody else.
        (name, unsafe { OwnedHandle::from_raw_handle(raw) })
    }

    /// The app's Stop: once the event is set, the flag and SIGTERM.
    #[test]
    fn a_set_cancel_event_is_sigterm() {
        use windows_sys::Win32::System::Threading::SetEvent;
        let (name, handle) = event(false);
        install().unwrap();
        watch_event(&name).unwrap();
        assert!(!CANCEL.load(Ordering::SeqCst), "not before it is set");
        // SAFETY: the event is open for the call.
        assert_ne!(unsafe { SetEvent(handle.as_raw_handle()) }, 0);
        let started = std::time::Instant::now();
        while !CANCEL.load(Ordering::SeqCst) && started.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(received(), Some(SIGTERM));
    }

    /// An event already set stops the run at once, before its first item.
    #[test]
    fn a_cancel_event_already_set_stops_at_once() {
        let (name, _handle) = event(true);
        install().unwrap();
        watch_event(&name).unwrap();
        assert!(CANCEL.load(Ordering::SeqCst) && received() == Some(SIGTERM));
    }

    #[test]
    fn a_cancel_event_nobody_created_is_an_error() {
        let e = watch_event("ddc-test-no-such-event").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
        assert!(e.to_string().contains("no such event"), "{e}");
    }

    #[test]
    fn a_group_nobody_published_is_not_found() {
        assert_eq!(signal_group(4242, SIGKILL).unwrap_err().kind(), io::ErrorKind::NotFound);
        kill_group(4242, SIGKILL);
    }
}
