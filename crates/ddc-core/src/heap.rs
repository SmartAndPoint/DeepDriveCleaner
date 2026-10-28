//! A dhat heap profile of one run (story "Memory and CPU under load: peak
//! memory, leaks and CPU time"). With the `dhat-heap` feature, dhat's
//! allocator is the global one and [`start`] begins the profile; it is
//! written to `dhat-heap.json` in the working directory when the returned
//! guard drops at the end of `main`. Without the feature, [`start`] does
//! nothing, so the CLI calls it unconditionally.

#[cfg(all(feature = "dhat-heap", feature = "op-stats"))]
compile_error!("`dhat-heap` and `op-stats` each install a global allocator: build with one of them");

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

/// The running profile: keep it for the whole run.
#[must_use = "the profile is written when this drops"]
pub struct Profile {
    #[cfg(feature = "dhat-heap")]
    _profiler: dhat::Profiler,
}

/// Starts the heap profile; call it first in `main`.
pub fn start() -> Profile {
    Profile {
        #[cfg(feature = "dhat-heap")]
        _profiler: dhat::Profiler::new_heap(),
    }
}
