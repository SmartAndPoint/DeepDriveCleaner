//! The harness folders' sizes, through the size-only visitor of `ddc disk`:
//! each folder is a region, a hard link counts once, and nothing is claimed.
//! It also sums the transcripts no live session writes, and each Claude
//! Code session's bytes by kind, from the entries the walk lists anyway; no
//! transcript is opened.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use crate::disk::visitor::DiskVisitor;
use crate::rules::Catalog;
use crate::rules::entries::{EntryShape, SessionPart, session_part};
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

/// The kinds of a session's bytes, as indexes of [`FootprintBytes`]' counters.
pub(super) const TRANSCRIPT: usize = 0;
pub(super) const CHECKPOINTS: usize = 1;
pub(super) const TEMP: usize = 2;

/// Each Claude Code session's bytes by kind, for the sessions found before
/// the walk: its transcript and companions under `projects`, its
/// checkpoints under `history`, its folder under `temp`. Lock-free: one
/// counter per session and kind.
pub(super) struct FootprintBytes {
    /// Canonical `~/.claude/projects`.
    projects: PathBuf,
    /// Canonical `~/.claude/file-history`.
    history: PathBuf,
    /// Claude Code's temporary folder, canonical; `None` when absent or skipped.
    temp: Option<PathBuf>,
    index: HashMap<String, usize>,
    counts: Vec<[AtomicU64; 3]>,
}

impl FootprintBytes {
    pub fn new(projects: PathBuf, history: PathBuf, temp: Option<PathBuf>, ids: &[&str]) -> Self {
        let index = ids.iter().enumerate().map(|(i, id)| ((*id).to_owned(), i)).collect();
        let counts = ids.iter().map(|_| [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)]).collect();
        Self { projects, history, temp, index, counts }
    }

    /// `id`'s bytes, by kind.
    pub fn of(&self, id: &str) -> [u64; 3] {
        self.index.get(id).map_or([0; 3], |&i| self.counts[i].each_ref().map(|c| c.load(Relaxed)))
    }

    fn count(&self, dir: &Path, name: &OsStr, alloc: u64) {
        fn part(name: &OsStr, is_dir: bool) -> Option<&str> {
            match session_part(EntryShape::Transcript, name.as_encoded_bytes(), is_dir) {
                SessionPart::Key(id) | SessionPart::Companion(id) => Some(id),
                SessionPart::None => None,
            }
        }
        fn uuid(c: &OsStr) -> Option<&str> {
            c.to_str().filter(|s| is_uuid(s))
        }
        let found = if let Ok(rel) = dir.strip_prefix(&self.projects) {
            let rel: Vec<&OsStr> = rel.iter().collect();
            match rel.as_slice() {
                [_] => part(name, false),
                [_, companion, ..] => part(companion, true),
                [] => None,
            }
            .map(|id| (TRANSCRIPT, id))
        } else if let Ok(rel) = dir.strip_prefix(&self.history) {
            rel.iter().next().and_then(uuid).map(|id| (CHECKPOINTS, id))
        } else if let Some(rel) = self.temp.as_deref().and_then(|t| dir.strip_prefix(t).ok()) {
            rel.iter().nth(1).and_then(uuid).map(|id| (TEMP, id))
        } else {
            None
        };
        if let Some((kind, id)) = found
            && let Some(&i) = self.index.get(id)
        {
            self.counts[i][kind].fetch_add(alloc, Relaxed);
        }
    }
}

struct AgentsVisitor<'a> {
    inner: DiskVisitor<'a>,
    transcripts: &'a [TranscriptBytes],
    footprint: Option<&'a FootprintBytes>,
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
        if let Some(f) = self.footprint {
            f.count(dir, &entry.name, entry.alloc);
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
    footprint: Option<&FootprintBytes>,
    threads: Option<usize>,
    cancel: &AtomicBool,
) -> Vec<Size> {
    let regions: Vec<(PathBuf, Tag)> = folders.iter().enumerate().map(|(i, (p, _))| (p.clone(), i as Tag)).collect();
    let mut roots: Vec<PathBuf> = folders.iter().filter(|(_, nested)| !nested).map(|(p, _)| p.clone()).collect();
    roots.dedup();
    let visitor = AgentsVisitor {
        inner: DiskVisitor::new(catalog, user_skips, &regions, vec![false; folders.len()], u64::MAX, cancel),
        transcripts,
        footprint,
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
