//! Parallel, rule-agnostic filesystem walk. Rules plug in through [`Visitor`]:
//! a directory can be claimed whole (a finding), marked as a region (rules
//! keep working inside, the remainder belongs to the region), skipped, or
//! descended into.

mod inodes;
mod progress;
pub mod reader;
mod walker;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use progress::{Progress, ProgressSnapshot};
pub use reader::{DirListing, DirReader, EntryKind, EntryMeta, ReadError, ReadErrorKind, StdReader};
pub use walker::{walk, walk_with_reader};

/// Rule identifier chosen by the visitor (opaque to the walker).
pub type Tag = u32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OwnerKind {
    /// The whole subtree is one finding; rules are not applied inside.
    Claim,
    /// Rules still apply inside; bytes not claimed by them belong here.
    Region,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum SkipReason {
    /// Excluded by the visitor (`--exclude`, cloud folders, …).
    Excluded,
    /// Evicted cloud content; reading it would download it.
    Dataless,
    /// Another filesystem is mounted here.
    OtherFilesystem,
    /// The directory changed between listing its parent and opening it.
    Changed,
}

/// Decision taken before a directory is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pre {
    Continue,
    Claim(Tag),
    Region(Tag),
    Skip(SkipReason),
}

/// Decision taken after a directory is read (its own entries are known).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Post {
    Descend,
    Claim(Tag),
}

/// What the visitor knows about a directory.
#[derive(Debug, Clone, Copy)]
pub struct DirCtx<'a> {
    pub path: &'a Path,
    pub name: &'a OsStr,
    pub depth: u32,
    pub dev: u64,
    pub ino: u64,
    /// The finding or region this directory already belongs to.
    pub owner: Option<(OwnerKind, Tag)>,
}

/// Rule hooks. Not called inside a [`Pre::Claim`]/[`Post::Claim`] subtree.
pub trait Visitor: Sync {
    fn before_read(&self, _dir: &DirCtx<'_>) -> Pre {
        Pre::Continue
    }

    fn after_read(&self, _dir: &DirCtx<'_>, _listing: &DirListing) -> Post {
        Post::Descend
    }

    /// Whether a claim needs APFS private sizes. They roughly triple the cost
    /// of listing, so visitors ask only where clones are expected.
    fn wants_private(&self, _dir: &DirCtx<'_>, _tag: Tag) -> bool {
        true
    }

    /// Called for every non-directory entry, including inside claims.
    fn on_file(&self, _dir: &Path, _entry: &EntryMeta, _owner: Option<(OwnerKind, Tag)>) {}
}

/// Visitor that only measures.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullVisitor;

impl Visitor for NullVisitor {}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReaderKind {
    #[default]
    Bulk,
    Std,
}

#[derive(Debug, Clone)]
pub struct WalkOptions {
    /// Worker threads; `None` = available parallelism.
    pub threads: Option<usize>,
    pub reader: ReaderKind,
    /// Request APFS private size for every file, not only inside claims.
    pub private_everywhere: bool,
    /// Errors and skips beyond this many are counted but not listed.
    pub max_recorded: usize,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self { threads: None, reader: ReaderKind::Bulk, private_everywhere: false, max_recorded: 10_000 }
    }
}

/// A claimed finding or a region after the walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerResult {
    pub tag: Tag,
    pub kind: OwnerKind,
    pub path: PathBuf,
    pub dev: u64,
    pub ino: u64,
    /// Identity of the parent directory at scan time.
    pub parent_dev: u64,
    pub parent_ino: u64,
    /// Allocated bytes that belong to this owner alone.
    pub bytes: u64,
    /// Allocated bytes of hard-linked inodes that also have links elsewhere
    /// (another owner, outside any owner, or outside the scan).
    pub shared_bytes: u64,
    /// Lower bound of what deleting this owner frees (APFS private size of
    /// its exclusive files). `None` when not measured or not available for
    /// some file (non-APFS volume, read failure).
    pub private_bytes: Option<u64>,
    pub files: u64,
    pub dirs: u64,
    /// Unreadable directories or entries inside; the size is then incomplete.
    pub errors: u64,
    /// Subdirectories skipped (dataless, other filesystem, changed) or
    /// vanished inside; the size is then incomplete.
    pub skipped: u64,
}

impl OwnerResult {
    /// Whether some content could not be measured.
    pub fn incomplete(&self) -> bool {
        self.errors > 0 || self.skipped > 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootResult {
    pub path: PathBuf,
    /// Allocated bytes under this root, each inode counted once (like `du -skx`).
    pub bytes: u64,
    pub files: u64,
    pub dirs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkError {
    pub path: PathBuf,
    pub error: ReadError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub path: PathBuf,
    pub reason: SkipReason,
}

#[derive(Debug, Clone)]
pub struct WalkOutput {
    pub roots: Vec<RootResult>,
    /// Sorted by path.
    pub owners: Vec<OwnerResult>,
    /// All allocated bytes, each inode once.
    pub total_bytes: u64,
    /// Bytes outside every claim and region.
    pub unowned_bytes: u64,
    /// Sorted by path; at most [`WalkOptions::max_recorded`].
    pub errors: Vec<WalkError>,
    pub errors_total: u64,
    /// Sorted by path; at most [`WalkOptions::max_recorded`].
    pub skipped: Vec<Skipped>,
    pub skipped_total: u64,
    /// Entries that disappeared while the walk was running.
    pub vanished: u64,
    pub cancelled: bool,
    pub elapsed: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum WalkSetupError {
    #[error("cannot open scan root {path}: {source}")]
    Root {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot start worker threads: {0}")]
    Pool(#[from] rayon::ThreadPoolBuildError),
}
