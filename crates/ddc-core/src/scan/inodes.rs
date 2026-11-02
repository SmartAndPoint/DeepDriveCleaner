//! Deferred accounting for hard-linked files (`nlink > 1`; a reader that
//! cannot read a count reports `u32::MAX`, which never resolves complete).
//! During the walk every sighting is only recorded, keeping the largest
//! count any sighting gave; after the walk each inode is resolved once,
//! independent of thread timing.

use std::collections::HashMap;
use std::sync::Mutex;

const SHARDS: usize = 64;

/// Where a link was seen: scan root and (optionally) the owning finding and
/// the listed entry it lies in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Holder {
    pub root: usize,
    pub owner: Option<usize>,
    pub entry: Option<usize>,
}

#[derive(Debug)]
struct Rec {
    alloc: u64,
    private: Option<u64>,
    nlink: u32,
    seen: u32,
    holders: Vec<Holder>,
}

/// One inode after the walk.
#[derive(Debug)]
pub(crate) struct Resolved {
    pub alloc: u64,
    pub private: Option<u64>,
    /// Every link of the inode was seen inside the scan.
    pub complete: bool,
    /// Distinct holders, sorted.
    pub holders: Vec<Holder>,
}

pub(crate) struct InodeTable {
    shards: Vec<Mutex<HashMap<(u64, u64), Rec>>>,
}

impl InodeTable {
    pub fn new() -> Self {
        Self { shards: (0..SHARDS).map(|_| Mutex::new(HashMap::new())).collect() }
    }

    pub fn record(&self, dev: u64, ino: u64, alloc: u64, private: Option<u64>, nlink: u32, holder: Holder) {
        let shard = &self.shards[(ino as usize ^ dev as usize) % SHARDS];
        let mut map = shard.lock().unwrap_or_else(|p| p.into_inner());
        let rec = map.entry((dev, ino)).or_insert_with(|| Rec { alloc, private, nlink, seen: 0, holders: Vec::new() });
        // The largest count wins, whichever sighting came first: a link made
        // during the walk, or one count that could not be read (`u32::MAX`).
        rec.nlink = rec.nlink.max(nlink);
        rec.seen += 1;
        if !rec.holders.contains(&holder) {
            rec.holders.push(holder);
        }
    }

    pub fn resolve(self) -> Vec<Resolved> {
        let mut out = Vec::new();
        for shard in self.shards {
            for (_, mut rec) in shard.into_inner().unwrap_or_else(|p| p.into_inner()) {
                rec.holders.sort();
                out.push(Resolved {
                    alloc: rec.alloc,
                    private: rec.private,
                    complete: rec.seen >= rec.nlink,
                    holders: rec.holders,
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn holder(root: usize) -> Holder {
        Holder { root, owner: None, entry: None }
    }

    fn complete(sightings: &[u32]) -> bool {
        let t = InodeTable::new();
        for (i, n) in sightings.iter().enumerate() {
            t.record(1, 7, 4096, None, *n, holder(i));
        }
        let r = t.resolve();
        assert_eq!(r.len(), 1);
        r[0].complete
    }

    #[test]
    fn every_link_seen_is_complete() {
        assert!(complete(&[2, 2]));
        assert!(!complete(&[2]), "one link outside the scan");
    }

    #[test]
    fn an_unreadable_count_is_never_complete_in_any_order() {
        assert!(!complete(&[u32::MAX]));
        assert!(!complete(&[u32::MAX, 2]));
        assert!(!complete(&[2, u32::MAX]), "the larger count wins, whichever came first");
        assert!(!complete(&[2, 3]) && !complete(&[3, 2]), "a link made during the walk");
    }
}
