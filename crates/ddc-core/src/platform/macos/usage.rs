//! CPU time and peak memory of a child, of this process and of the calling
//! thread, for the load harness (story "Memory and CPU under load"). `rustix`
//! has no `wait4` or `getrusage`, so they live here with the other `unsafe`.

use std::ffi::CString;
use std::io;
use std::mem::MaybeUninit;
use std::os::unix::process::ExitStatusExt as _;
use std::process::ExitStatus;
use std::time::Duration;

pub use crate::platform::types::Usage;

/// `ru_maxrss` is in bytes on macOS and in KiB on Linux.
#[cfg(target_os = "macos")]
const RSS_UNIT: u64 = 1;
#[cfg(not(target_os = "macos"))]
const RSS_UNIT: u64 = 1024;

fn from_rusage(ru: &libc::rusage) -> Usage {
    Usage {
        user: duration(ru.ru_utime),
        system: duration(ru.ru_stime),
        max_rss_bytes: u64::try_from(ru.ru_maxrss).unwrap_or(0) * RSS_UNIT,
    }
}

fn duration(tv: libc::timeval) -> Duration {
    Duration::from_secs(u64::try_from(tv.tv_sec).unwrap_or(0))
        + Duration::from_micros(u64::try_from(tv.tv_usec).unwrap_or(0))
}

/// Reaps the child `pid` and returns its status and its own usage (its reaped
/// children's included). With `nohang`, `None` while it still runs. The caller
/// must not wait on it any other way: `std::process::Child::wait` would reap it
/// first, and this would fail with `ECHILD`.
pub fn wait4(pid: u32, nohang: bool) -> io::Result<Option<(ExitStatus, Usage)>> {
    // Pid 0 would reap any child of the process group.
    let pid = libc::pid_t::try_from(pid).ok().filter(|&p| p > 0).ok_or(io::ErrorKind::InvalidInput)?;
    let options = if nohang { libc::WNOHANG } else { 0 };
    loop {
        let mut status = 0;
        let mut ru = MaybeUninit::<libc::rusage>::zeroed();
        // SAFETY: `status` and `ru` are valid out-pointers.
        let r = unsafe { libc::wait4(pid, &mut status, options, ru.as_mut_ptr()) };
        if r == 0 {
            return Ok(None);
        }
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        // SAFETY: filled by the kernel when `wait4` returned the pid.
        let ru = unsafe { ru.assume_init() };
        return Ok(Some((ExitStatus::from_raw(status), from_rusage(&ru))));
    }
}

/// This process's usage so far.
pub fn self_usage() -> io::Result<Usage> {
    let mut ru = MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `ru` is a valid out-pointer.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, ru.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: filled by the kernel on success.
    Ok(from_rusage(&unsafe { ru.assume_init() }))
}

/// CPU time of the calling thread, unaffected by other threads of the process.
pub fn thread_cpu_time() -> io::Result<Duration> {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid out-pointer.
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Duration::new(u64::try_from(ts.tv_sec).unwrap_or(0), u32::try_from(ts.tv_nsec).unwrap_or(0)))
}

/// The 1-minute load average.
pub fn load_average() -> Option<f64> {
    let mut avg = [0f64; 1];
    // SAFETY: `avg` holds the 1 sample asked for.
    let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 1) };
    (n == 1).then_some(avg[0])
}

/// The machine model (`hw.model`), such as `Mac16,8`.
pub fn machine_model() -> Option<String> {
    let name = CString::new("hw.model").ok()?;
    let mut size = 0usize;
    // SAFETY: a size query: a null buffer and a valid out-pointer for the size.
    let r = unsafe { libc::sysctlbyname(name.as_ptr(), std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) };
    if r != 0 || size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size];
    // SAFETY: `buf` is writable for `size` bytes.
    let r = unsafe { libc::sysctlbyname(name.as_ptr(), buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) };
    if r != 0 {
        return None;
    }
    buf.truncate(size);
    let text = String::from_utf8(buf).ok()?;
    let text = text.trim_end_matches('\0');
    (!text.is_empty()).then(|| text.to_owned())
}

#[cfg(test)]
mod tests {
    use std::process::Command;
    use std::time::Instant;

    use super::*;

    /// Busy until this thread has used `cpu`; a busy machine only makes it take longer.
    fn spin(cpu: Duration) {
        let (start, deadline) = (thread_cpu_time().unwrap(), Instant::now() + Duration::from_secs(30));
        let mut x = 0u64;
        while thread_cpu_time().unwrap() - start < cpu {
            assert!(Instant::now() < deadline, "no CPU in 30 s");
            for _ in 0..10_000 {
                x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(7));
            }
        }
    }

    // The children are reaped by `wait4`, which clippy cannot see.
    #[test]
    #[allow(clippy::zombie_processes)]
    fn a_reaped_child_reports_its_status_and_its_peak_memory() {
        let child = Command::new("/bin/sh").args(["-c", "exit 3"]).spawn().unwrap();
        let (status, usage) = wait4(child.id(), false).unwrap().expect("reaped");
        assert_eq!(status.code(), Some(3));
        assert!(usage.max_rss_bytes > 100 * 1024, "{usage:?}");
    }

    #[test]
    #[allow(clippy::zombie_processes)]
    fn a_running_child_is_not_reaped_without_waiting() {
        use std::os::unix::process::ExitStatusExt as _;
        let mut child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        assert!(wait4(child.id(), true).unwrap().is_none());
        // `kill` signals without reaping.
        child.kill().unwrap();
        let (status, _) = wait4(child.id(), false).unwrap().expect("reaped");
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        assert!(wait4(child.id(), true).is_err(), "already reaped");
        assert_eq!(wait4(0, true).unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn busy_time_of_a_thread_shows_in_the_process() {
        let before = self_usage().unwrap();
        spin(Duration::from_millis(200));
        let after = self_usage().unwrap();
        assert!(after.cpu() - before.cpu() >= Duration::from_millis(200), "{before:?} {after:?}");
        assert!(after.max_rss_bytes > 0);
    }

    #[test]
    fn a_sleeping_thread_uses_no_cpu() {
        let before = thread_cpu_time().unwrap();
        std::thread::sleep(Duration::from_millis(200));
        assert!(thread_cpu_time().unwrap() - before < Duration::from_millis(5));
    }

    #[test]
    fn the_load_average_and_the_model_are_read() {
        assert!(load_average().is_some_and(|l| l >= 0.0));
        let model = machine_model().expect("hw.model");
        assert!(!model.is_empty() && !model.contains('\0'), "{model:?}");
    }
}
