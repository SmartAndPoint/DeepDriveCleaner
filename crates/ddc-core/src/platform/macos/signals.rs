//! SIGINT, SIGTERM and SIGHUP during a clean (story "CLI: clean an explicit item list").
//!
//! The handler only stores the signal and forwards it to the native command
//! running in its own process group: a command spawned with
//! `process_group(0)` has left the terminal's foreground group, so without
//! the forwarding Ctrl-C would no longer reach it, nor the hangup of a closed
//! terminal. The executor checks the
//! flag between items and stops cooperatively; ddc then ends by the signal
//! through [`reraise`], so a parent sees a death by signal, not an exit code.

use std::io;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// Ends a process at once; it cannot be caught.
pub const SIGKILL: i32 = libc::SIGKILL;

static CANCEL: AtomicBool = AtomicBool::new(false);
static SIGNAL: AtomicI32 = AtomicI32::new(0);
/// Process group of the native command running now; 0 when none. One slot:
/// the executor runs one command at a time.
static CHILD_GROUP: AtomicI32 = AtomicI32::new(0);
/// Said at once: the current item, a long delete included, runs to its end.
const NOTICE: &[u8] = b"\nddc: stopping after the current item\n";

extern "C" fn on_signal(sig: libc::c_int) {
    // SAFETY: `__error` returns this thread's errno; saved and restored so the
    // interrupted code never sees the `kill` below change it.
    let errno = unsafe { *libc::__error() };
    SIGNAL.store(sig, Ordering::SeqCst);
    CANCEL.store(true, Ordering::SeqCst);
    let group = CHILD_GROUP.load(Ordering::SeqCst);
    // Never 0: `kill(0, sig)` would signal ddc's own group (under a test runner, the runner).
    if group > 0 {
        // SAFETY: kill(2) is async-signal-safe; a negative pid names a process group.
        unsafe { libc::kill(-group, sig) };
    }
    // SAFETY: write(2) is async-signal-safe; the buffer is static. A failed
    // write (a closed terminal) is ignored.
    unsafe { libc::write(libc::STDERR_FILENO, NOTICE.as_ptr().cast(), NOTICE.len()) };
    // SAFETY: as above.
    unsafe { *libc::__error() = errno };
}

/// Installs the handler for SIGINT, SIGTERM and SIGHUP and returns the flag it sets.
/// `SA_RESTART`: reads and writes in flight are resumed, not failed with `EINTR`.
pub fn install() -> io::Result<&'static AtomicBool> {
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: an all-zero `sigaction` is valid; the fields are set below.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        action.sa_sigaction = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        // SAFETY: `sa_mask` is a valid sigset_t to initialise; the pointers are valid.
        let failed = unsafe {
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(sig, &action, std::ptr::null_mut()) != 0
        };
        if failed {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(&CANCEL)
}

/// The signal the handler received, if any.
pub fn received() -> Option<i32> {
    match SIGNAL.load(Ordering::SeqCst) {
        0 => None,
        sig => Some(sig),
    }
}

/// Publishes the process group of a native command just spawned. If a signal
/// arrived before it was published, it is forwarded now, so it is never lost.
pub fn set_child_group(group: i32) {
    CHILD_GROUP.store(group, Ordering::SeqCst);
    if let Some(sig) = received() {
        kill_group(group, sig);
    }
}

pub fn clear_child_group() {
    CHILD_GROUP.store(0, Ordering::SeqCst);
}

/// Sends `sig` to every process of `group` (a positive process group id).
pub fn kill_group(group: i32, sig: i32) {
    let _ = signal_group(group, sig);
}

/// Sends `sig` to every process of `group` (a positive process group id);
/// the error when there is no such group or it refuses the signal (macOS
/// refuses to signal a group that holds only a zombie).
pub fn signal_group(group: i32, sig: i32) -> io::Result<()> {
    if group <= 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("not a process group: {group}")));
    }
    // SAFETY: kill(2) with a negative pid signals a process group; no memory is involved.
    if unsafe { libc::kill(-group, sig) } == 0 { Ok(()) } else { Err(io::Error::last_os_error()) }
}

/// Ends the process by `sig`: restores its default action, unblocks it and
/// raises it. Call after the report is written and stdout flushed.
pub fn reraise(sig: i32) -> ! {
    // SAFETY: restoring SIG_DFL and unblocking a signal are plain libc calls on
    // valid arguments; raise(3) then delivers it to this thread.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, sig);
        libc::pthread_sigmask(libc::SIG_UNBLOCK, &set, std::ptr::null_mut());
        libc::raise(sig);
    }
    // A signal whose default is to end the process does not return here.
    std::process::exit(128 + sig)
}
