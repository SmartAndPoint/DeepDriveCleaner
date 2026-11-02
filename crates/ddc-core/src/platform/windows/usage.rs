//! Resource usage on Windows (story "Windows scanner and rule catalog",
//! step 7): a process's user and kernel times (`GetProcessTimes`) and its
//! peak working set (`GetProcessMemoryInfo`), the counterpart of `rusage`'s
//! `ru_maxrss`. Windows has no load average, and the machine's model is not
//! read.

use std::io;
use std::os::windows::io::{FromRawHandle as _, OwnedHandle};
use std::os::windows::process::ExitStatusExt as _;
use std::process::ExitStatus;
use std::time::Duration;

use windows_sys::Win32::Foundation::{FILETIME, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, GetExitCodeProcess, GetProcessTimes, GetThreadTimes, INFINITE, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};

pub use crate::platform::types::Usage;

/// Waits for the process `pid` to end (with `nohang`, `None` while it still
/// runs) and returns its exit status and its own usage. Unlike Unix, the
/// process is not reaped: its handle stays valid for std's `Child` as well.
/// A child's own children are not counted; the command runner's Job Object
/// accounts for them (step 9). The caller keeps the process's handle (its
/// `Child`) open until this returns `Some`: Windows keeps a pid reserved
/// only while a handle is open, so the pid of a process nobody holds may name
/// another process. A second call returns the same status again.
pub fn wait4(pid: u32, nohang: bool) -> io::Result<Option<(ExitStatus, Usage)>> {
    if pid == 0 {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    // SAFETY: no pointers; a null handle is the error case, checked below.
    let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcess returned a handle nobody else owns.
    let process = unsafe { OwnedHandle::from_raw_handle(raw) };
    // SAFETY: the handle is open for the call.
    match unsafe { WaitForSingleObject(raw, if nohang { 0 } else { INFINITE }) } {
        WAIT_OBJECT_0 => {}
        WAIT_TIMEOUT => return Ok(None),
        _ => return Err(io::Error::last_os_error()),
    }
    let mut code = 0u32;
    // SAFETY: the handle is open; `code` is a writable local.
    if unsafe { GetExitCodeProcess(raw, &mut code) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let usage = usage_of(raw)?;
    drop(process);
    Ok(Some((ExitStatus::from_raw(code), usage)))
}

/// This process's usage so far.
pub fn self_usage() -> io::Result<Usage> {
    // SAFETY: the pseudo-handle of the current process; it needs no closing.
    usage_of(unsafe { GetCurrentProcess() })
}

/// The CPU time (user plus kernel) of the calling thread.
pub fn thread_cpu_time() -> io::Result<Duration> {
    let (mut created, mut exited, mut kernel, mut user) = (zero(), zero(), zero(), zero());
    // SAFETY: the pseudo-handle of the current thread; four writable locals.
    if unsafe { GetThreadTimes(GetCurrentThread(), &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(duration(kernel) + duration(user))
}

/// Windows keeps no load average.
pub fn load_average() -> Option<f64> {
    None
}

/// Not read on Windows.
pub fn machine_model() -> Option<String> {
    None
}

fn usage_of(process: HANDLE) -> io::Result<Usage> {
    let (mut created, mut exited, mut kernel, mut user) = (zero(), zero(), zero(), zero());
    // SAFETY: an open process handle; four writable locals.
    if unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: PROCESS_MEMORY_COUNTERS is plain data; all zeroes is valid.
    let mut memory: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    let size = u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS>()).expect("a small struct");
    // SAFETY: an open process handle; the output struct is a writable local
    // of the stated size.
    let peak = if unsafe { K32GetProcessMemoryInfo(process, &mut memory, size) } != 0 {
        memory.PeakWorkingSetSize as u64
    } else {
        0
    };
    Ok(Usage { user: duration(user), system: duration(kernel), max_rss_bytes: peak })
}

fn zero() -> FILETIME {
    FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }
}

/// A FILETIME span, in 100-nanosecond units.
fn duration(t: FILETIME) -> Duration {
    let ticks = (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime);
    Duration::from_nanos(ticks.saturating_mul(100))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_has_times_and_a_peak_working_set() {
        let start = thread_cpu_time().unwrap();
        let mut x = 0u64;
        for i in 0..20_000_000u64 {
            x = x.wrapping_add(std::hint::black_box(i) * i);
        }
        assert_ne!(x, 1);
        assert!(thread_cpu_time().unwrap() > start, "the thread's time grew");
        let u = self_usage().unwrap();
        assert!(u.cpu() > Duration::ZERO && u.max_rss_bytes > 1 << 20, "{u:?}");
        assert_eq!((load_average(), machine_model()), (None, None));
    }

    #[test]
    fn a_child_is_waited_for_with_its_exit_code_and_usage() {
        let mut child = std::process::Command::new("cmd").args(["/C", "exit 7"]).spawn().unwrap();
        let (status, usage) = wait4(child.id(), false).unwrap().unwrap();
        assert_eq!(status.code(), Some(7));
        assert!(usage.max_rss_bytes > 0, "{usage:?}");
        assert_eq!(child.wait().unwrap().code(), Some(7), "std's Child can still wait: not reaped");

        let mut long = std::process::Command::new("ping")
            .args(["-n", "5", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        assert!(wait4(long.id(), true).unwrap().is_none(), "still running");
        long.kill().unwrap();
        long.wait().unwrap();
        assert!(wait4(0, false).is_err());
    }
}
