//! The counters of [`crate::stats`] and the counting global allocator,
//! only under the `op-stats` feature. Relaxed atomics, each on its own cache
//! line: the walker's threads add to them at once. Nothing here allocates.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::stats::Counter;

#[repr(align(128))]
struct Slot(AtomicU64);

static SLOTS: [Slot; Counter::ALL.len()] = [const { Slot(AtomicU64::new(0)) }; Counter::ALL.len()];

/// In every binary built with the counters, and in no other: the release
/// and dist builds check for its absence with `nm`.
#[used]
#[unsafe(no_mangle)]
pub static DDC_OP_STATS: u8 = 1;

#[inline(always)]
pub(crate) fn add(i: usize, n: u64) {
    SLOTS[i].0.fetch_add(n, Ordering::Relaxed);
}

pub(crate) fn get(i: usize) -> u64 {
    SLOTS[i].0.load(Ordering::Relaxed)
}

fn allocated(bytes: usize) {
    add(Counter::Allocations as usize, 1);
    add(Counter::BytesAllocated as usize, bytes as u64);
}

struct Counting;

// SAFETY: every call is forwarded unchanged to `System`; the counting
// around it neither allocates nor touches the memory.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocated(layout.size());
        // SAFETY: the caller's contract for `alloc`, passed on.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        allocated(layout.size());
        // SAFETY: the caller's contract for `alloc_zeroed`, passed on.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        allocated(new_size);
        // SAFETY: the caller's contract for `realloc`, passed on.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller's contract for `dealloc`, passed on.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;
