//! Live counters for progress display and a cancellation flag.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

#[derive(Debug, Default)]
pub struct Progress {
    files: AtomicU64,
    dirs: AtomicU64,
    bytes: AtomicU64,
    errors: AtomicU64,
    cancel: AtomicBool,
}

/// Point-in-time copy of [`Progress`]. `bytes` is approximate until hard
/// links are resolved at the end of the walk.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ProgressSnapshot {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
    pub errors: u64,
}

impl Progress {
    pub fn snapshot(&self) -> ProgressSnapshot {
        ProgressSnapshot {
            files: self.files.load(Relaxed),
            dirs: self.dirs.load(Relaxed),
            bytes: self.bytes.load(Relaxed),
            errors: self.errors.load(Relaxed),
        }
    }

    /// Asks the walk to stop; directories already queued return immediately.
    pub fn cancel(&self) {
        self.cancel.store(true, Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Relaxed)
    }

    pub(crate) fn add(&self, files: u64, dirs: u64, bytes: u64, errors: u64) {
        self.files.fetch_add(files, Relaxed);
        self.dirs.fetch_add(dirs, Relaxed);
        self.bytes.fetch_add(bytes, Relaxed);
        if errors > 0 {
            self.errors.fetch_add(errors, Relaxed);
        }
    }
}
