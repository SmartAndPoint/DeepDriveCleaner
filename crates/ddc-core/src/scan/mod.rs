//! Parallel, rule-agnostic filesystem walk. Rules plug in through [`Visitor`]:
//! a directory can be claimed whole (a finding), marked as a region (rules
//! keep working inside, the remainder belongs to the region), skipped, or
//! descended into.

mod inodes;
mod progress;
pub mod reader;
mod walker;

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use progress::{Progress, ProgressSnapshot};
pub use reader::{
    DirListing, DirReader, EntryKind, EntryMeta, ReadError, ReadErrorKind, StdReader, SystemReader, Times,
};
pub use walker::{normalized_roots, walk, walk_with_reader};

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

    /// Whether a claim reads its entries' modification times for
    /// [`OwnerResult::modified`] (story "Review window: sort a group's rows by
    /// name, size or last change"). Asked when the claim is made; a region
    /// never has one. [`WalkOptions::claim_times`] off overrides it.
    fn wants_times(&self, _dir: &DirCtx<'_>, _tag: Tag) -> bool {
        false
    }

    /// Whether a [`Pre::Claim`] or [`Pre::Region`] measures each direct child
    /// on its own ([`WalkOutput::entries`]), its listing read with times
    /// (story "User data: pick your own files for the Trash"). Asked before
    /// the read; a [`Post::Claim`] never lists entries.
    fn lists_entries(&self, _dir: &DirCtx<'_>, _tag: Tag) -> bool {
        false
    }

    /// Called for every non-directory entry, including inside claims.
    /// `dir_id` is the `(dev, ino)` of the directory as the walk listed it.
    fn on_file(&self, _dir: &Path, _dir_id: (u64, u64), _entry: &EntryMeta, _owner: Option<(OwnerKind, Tag)>) {}

    /// Whether the walk leaves `dir` unread: asked before every directory is
    /// opened, inside claims too (story "Faster clean: re-check only what the
    /// selection needs"). A pruned directory counts nowhere: in no owner,
    /// entry or root, and not as skipped; [`WalkOutput::pruned`] counts it.
    fn prunes(&self, _dir: &Path) -> bool {
        false
    }
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
    /// Read the modification times inside the claims whose visitor
    /// [wants them](Visitor::wants_times). Off only to measure what they
    /// cost (`ddc scan --no-times`); a listed folder's own entry times
    /// ([`Visitor::lists_entries`]) are read either way.
    pub claim_times: bool,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            threads: None,
            reader: ReaderKind::Bulk,
            private_everywhere: false,
            max_recorded: 10_000,
            claim_times: true,
        }
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
    /// The newest modification time inside, Unix seconds: of every entry
    /// listed without an error, each directory itself included. Only for a
    /// claim that [wants times](Visitor::wants_times); `None` when its root
    /// could not be read. A lower bound when it is incomplete, and when a
    /// [`Post::Claim`]'s root could not be listed again with times (which
    /// does not make it incomplete).
    pub modified: Option<i64>,
}

impl OwnerResult {
    /// Whether some content could not be measured.
    pub fn incomplete(&self) -> bool {
        self.errors > 0 || self.skipped > 0
    }
}

/// A direct child of an owner that lists entries, measured on its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryResult {
    pub name: OsString,
    pub kind: EntryKind,
    pub dev: u64,
    pub ino: u64,
    /// `st_flags` (`SF_DATALESS`, …).
    pub flags: u32,
    /// Errno of the child's own metadata; the other fields may be unset.
    pub error: Option<i32>,
    pub modified: Option<i64>,
    pub added: Option<i64>,
    /// Allocated bytes under it, claims inside included; a hard-linked file
    /// counts when every link of it lies inside this child.
    pub bytes: u64,
    /// Unreadable directories or entries inside.
    pub errors: u64,
    /// Subdirectories skipped (dataless, changed) or vanished inside.
    pub skipped: u64,
    /// Directories inside that belong to another filesystem.
    pub other_fs: u64,
}

/// The entries of one owner, sorted by name bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerEntries {
    pub owner: PathBuf,
    pub entries: Vec<EntryResult>,
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
    /// Per owner that lists entries ([`Visitor::lists_entries`]); sorted by path.
    pub entries: Vec<OwnerEntries>,
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
    /// Directories left unread by [`Visitor::prunes`].
    pub pruned: u64,
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
    /// The platform is not supported: nothing was opened (ADR "Platform
    /// seam: one platform module per OS with declared capabilities", §5).
    #[error("{0}")]
    Unsupported(crate::platform::Unsupported),
}
