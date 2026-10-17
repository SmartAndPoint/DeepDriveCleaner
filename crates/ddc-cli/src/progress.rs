//! Live progress line on stderr while a scan runs (terminals only).

use std::io::{IsTerminal as _, Write as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use ddc_core::scan::Progress;

use crate::size;

/// Runs `f`, drawing a spinner with counters from `progress` when stderr is a terminal.
pub fn with_spinner<T>(progress: &Progress, label: &str, f: impl FnOnce() -> T) -> T {
    if !std::io::stderr().is_terminal() {
        return f();
    }
    let done = AtomicBool::new(false);
    /// Stops the spinner even if `f` panics (the scope waits for the thread).
    struct Stop<'a>(&'a AtomicBool);
    impl Drop for Stop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }
    std::thread::scope(|s| {
        s.spawn(|| {
            const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            let mut i = 0;
            while !done.load(Ordering::Relaxed) {
                let snap = progress.snapshot();
                eprint!(
                    "\r\x1b[2K{} {label} {} files · {}",
                    FRAMES[i % FRAMES.len()],
                    size::count(snap.files),
                    size::human(snap.bytes)
                );
                let _ = std::io::stderr().flush();
                i += 1;
                std::thread::park_timeout(Duration::from_millis(120));
            }
            eprint!("\r\x1b[2K");
        });
        let _stop = Stop(&done);
        f()
    })
}
