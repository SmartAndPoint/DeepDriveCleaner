//! Deferred accounting for hard-linked files (`nlink > 1`). During the walk
//! every sighting is only recorded; after the walk each inode is resolved
//! once, independent of thread timing.

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
