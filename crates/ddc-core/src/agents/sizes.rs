//! The harness folders' sizes, through the size-only visitor of `ddc disk`:
//! each folder is a region, a hard link counts once, and nothing is claimed.
//! It also sums the transcripts no live session writes, from the entries
//! the walk lists anyway; no transcript is opened.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use crate::disk::visitor::DiskVisitor;
use crate::rules::Catalog;
use crate::scan::{self, DirCtx, DirListing, EntryMeta, OwnerKind, Post, Pre, Progress, Tag, Visitor, WalkOptions};

use super::codex::rollout_name;
use super::is_uuid;

/// Where a harness keeps its transcripts, and how they are named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Shape {
    /// `<p>/<id>.jsonl` and the `<p>/<id>/` folder of its subagents.
    Claude,
    /// `YYYY/MM/DD/rollout-<time>-<id>.jsonl`.
    Codex,
}

pub(super) struct TranscriptBytes {
    /// Canonical.
    pub root: PathBuf,
    pub shape: Shape,
    /// The sessions whose transcripts are left out.
    pub live: HashSet<String>,
    pub bytes: AtomicU64,
}

impl TranscriptBytes {
    pub fn new(root: PathBuf, shape: Shape, live: HashSet<String>) -> Self {
        Self { root, shape, live, bytes: AtomicU64::new(0) }
    }

    fn count(&self, dir: &Path, name: &OsStr, alloc: u64) {
        let Ok(rel) = dir.strip_prefix(&self.root) else { return };
        let rel: Vec<&OsStr> = rel.iter().collect();
        let name = name.to_str().unwrap_or_default();
        let id = match (self.shape, rel.as_slice()) {
            (Shape::Claude, [_]) => name.strip_suffix(".jsonl").filter(|s| is_uuid(s)),
            (Shape::Claude, [_, id, ..]) => id.to_str().filter(|s| is_uuid(s)),
            (Shape::Codex, [_, _, _]) => rollout_name(name).map(|(_, id)| id),
            _ => None,
        };
        if id.is_some_and(|id| !self.live.contains(id)) {
            self.bytes.fetch_add(alloc, Relaxed);
        }
    }
}

struct AgentsVisitor<'a> {
    inner: DiskVisitor<'a>,
    transcripts: &'a [TranscriptBytes],
}

impl Visitor for AgentsVisitor<'_> {
    fn before_read(&self, dir: &DirCtx<'_>) -> Pre {
        self.inner.before_read(dir)
    }

    fn after_read(&self, dir: &DirCtx<'_>, listing: &DirListing) -> Post {
        self.inner.after_read(dir, listing)
    }

    fn wants_private(&self, dir: &DirCtx<'_>, tag: Tag) -> bool {
        self.inner.wants_private(dir, tag)
    }

    fn on_file(&self, dir: &Path, _dir_id: (u64, u64), entry: &EntryMeta, _owner: Option<(OwnerKind, Tag)>) {
        for t in self.transcripts {
            t.count(dir, &entry.name, entry.alloc);
        }
    }

    fn prunes(&self, dir: &Path) -> bool {
        self.inner.prunes(dir)
    }
}

/// One folder's size: allocated bytes, and what could not be read inside.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Size {
    pub bytes: u64,
    pub unreadable: u64,
}

/// Measures `folders` (canonical, existing, not skipped; `true` when nested
/// in another of them), in their order.
pub(super) fn measure(
    folders: &[(PathBuf, bool)],
    catalog: &Catalog,
    user_skips: &[PathBuf],
    transcripts: &[TranscriptBytes],
    threads: Option<usize>,
    cancel: &AtomicBool,
) -> Vec<Size> {
    let regions: Vec<(PathBuf, Tag)> = folders.iter().enumerate().map(|(i, (p, _))| (p.clone(), i as Tag)).collect();
    let mut roots: Vec<PathBuf> = folders.iter().filter(|(_, nested)| !nested).map(|(p, _)| p.clone()).collect();
    roots.dedup();
    let visitor = AgentsVisitor {
        inner: DiskVisitor::new(catalog, user_skips, &regions, vec![false; folders.len()], u64::MAX, cancel),
        transcripts,
    };
    let opts = WalkOptions { threads, ..WalkOptions::default() };
    let Ok(out) = scan::walk(&roots, &visitor, &opts, &Progress::default()) else {
        return folders.iter().map(|_| Size { bytes: 0, unreadable: 1 }).collect();
    };
    folders
        .iter()
        .enumerate()
        .map(|(i, (path, nested))| {
            let owner = out.owners.iter().find(|o| o.tag == i as Tag);
            if *nested {
                return owner.map_or(Size { bytes: 0, unreadable: 1 }, |o| Size {
                    bytes: o.bytes + o.shared_bytes,
                    unreadable: o.errors,
                });
            }
            // A top-level folder holds its nested ones: the root's bytes,
            // each inode once.
            let unreadable = out.owners.iter().filter(|o| o.path.starts_with(path)).map(|o| o.errors).sum();
            let bytes = out
                .roots
                .iter()
                .find(|r| &r.path == path)
                .map(|r| r.bytes)
                .or(owner.map(|o| o.bytes + o.shared_bytes))
                .unwrap_or(0);
            Size { bytes, unreadable }
        })
        .collect()
}
