//! Cost counters on the hot paths, for the complexity guard (story "Complexity
//! guard: operation counts and scaling exponents", DDC-005). They count work,
//! not calls: every directory entry read, every comparison, every step up the
//! ancestors. A test reads them as a [`Snapshot`] delta around a scan at n and
//! 2n entries and checks each ratio against its [`Bound`].
//!
//! Only the `op-stats` feature counts; without it [`add`] compiles to nothing
//! and [`read_dir`] is `fs::read_dir`. The feature is never on in a release
//! build. Count at the cost site, inside the loop body or the comparison,
//! never once at the call: a restored quadratic step must still be counted.

use std::fs;
use std::io;
use std::path::Path;

/// How fast a counter may grow with the entries of the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    Linear,
    NLogN,
}

/// A kind of work. One counter, one kind of cost: a big linear site and a
/// small quadratic one under one counter hide the quadratic one at small n.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Counter {
    DirEntriesRead,
    ClaimLookups,
    RuleMatchAttempts,
    AncestorChecks,
    MarkerChecks,
    Allocations,
    BytesAllocated,
}

impl Counter {
    pub const ALL: [Counter; 7] = [
        Counter::DirEntriesRead,
        Counter::ClaimLookups,
        Counter::RuleMatchAttempts,
        Counter::AncestorChecks,
        Counter::MarkerChecks,
        Counter::Allocations,
        Counter::BytesAllocated,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Counter::DirEntriesRead => "dir_entries_read",
            Counter::ClaimLookups => "claim_lookups",
            Counter::RuleMatchAttempts => "rule_match_attempts",
            Counter::AncestorChecks => "ancestor_checks",
            Counter::MarkerChecks => "marker_checks",
            Counter::Allocations => "allocations",
            Counter::BytesAllocated => "bytes_allocated",
        }
    }

    /// Its bound and why.
    pub fn bound(self) -> (Bound, &'static str) {
        match self {
            Counter::DirEntriesRead => (Bound::Linear, "each directory is listed once per walk"),
            Counter::ClaimLookups => {
                (Bound::Linear, "one set lookup per claim; a scan of the findings only once per user-data folder")
            }
            Counter::RuleMatchAttempts => (
                Bound::Linear,
                "the matchers of one catalog per directory read; their child lookups scan a listing already counted in dir_entries_read",
            ),
            Counter::AncestorChecks => (Bound::Linear, "ancestors to the scan root, a bounded depth per finding"),
            Counter::MarkerChecks => {
                (Bound::Linear, "one set of the markers' ancestors, looked up once per claim or entry")
            }
            Counter::Allocations => (Bound::Linear, "a bounded number of buffers per entry"),
            Counter::BytesAllocated => {
                (Bound::Linear, "a bounded number of bytes per entry; a realloc counts its whole new size")
            }
        }
    }
}

/// Adds `n` to a counter.
#[inline(always)]
pub fn add(c: Counter, n: u64) {
    #[cfg(feature = "op-stats")]
    crate::sys::op_stats::add(c as usize, n);
    #[cfg(not(feature = "op-stats"))]
    let _ = (c, n);
}

/// `check()`, counted once in `c`: for a comparison inside a loop.
#[inline(always)]
pub fn counted(c: Counter, check: impl FnOnce() -> bool) -> bool {
    add(c, 1);
    check()
}

/// Every counter at one moment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Snapshot([u64; Counter::ALL.len()]);

impl Snapshot {
    pub fn get(&self, c: Counter) -> u64 {
        self.0[c as usize]
    }

    /// The work done since `before`.
    pub fn delta(&self, before: &Snapshot) -> Snapshot {
        Snapshot(std::array::from_fn(|i| self.0[i].saturating_sub(before.0[i])))
    }
}

/// The counters now; all zero without `op-stats`.
pub fn snapshot() -> Snapshot {
    #[cfg(feature = "op-stats")]
    {
        Snapshot(std::array::from_fn(crate::sys::op_stats::get))
    }
    #[cfg(not(feature = "op-stats"))]
    {
        Snapshot::default()
    }
}

/// `fs::read_dir` that counts every entry it yields in
/// [`Counter::DirEntriesRead`], as it yields it: a read that stops early
/// counts only what it read. Every directory read in the core goes through
/// it or through a `DirReader`.
pub fn read_dir(dir: impl AsRef<Path>) -> io::Result<ReadDir> {
    fs::read_dir(dir).map(ReadDir)
}

pub struct ReadDir(fs::ReadDir);

impl Iterator for ReadDir {
    type Item = io::Result<fs::DirEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        let entry = self.0.next();
        if entry.is_some() {
            add(Counter::DirEntriesRead, 1);
        }
        entry
    }
}

#[cfg(all(test, feature = "op-stats"))]
mod tests {
    use super::*;

    #[test]
    fn read_dir_counts_what_it_yields() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            std::fs::write(dir.path().join(i.to_string()), "").unwrap();
        }
        let before = snapshot();
        assert_eq!(read_dir(dir.path()).unwrap().count(), 5);
        assert_eq!(snapshot().delta(&before).get(Counter::DirEntriesRead), 5);
        let before = snapshot();
        assert!(read_dir(dir.path()).unwrap().next().is_some());
        assert_eq!(snapshot().delta(&before).get(Counter::DirEntriesRead), 1);
    }

    #[test]
    fn an_allocation_is_counted() {
        let before = snapshot();
        let v: Vec<u8> = std::hint::black_box(Vec::with_capacity(1000));
        let d = snapshot().delta(&before);
        drop(v);
        assert!(d.get(Counter::Allocations) >= 1, "{d:?}");
        assert!(d.get(Counter::BytesAllocated) >= 1000, "{d:?}");
    }
}
