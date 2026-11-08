//! The report `ddc agent footprint --json` prints (schema
//! `docs/footprint.schema.json`).

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::{Finding, Origin, WorktreeState};

/// Version of [`FootprintReport`].
pub const FOOTPRINT_SCHEMA_VERSION: u32 = 1;

/// One project's checkouts, what each holds, and what can go (ADR "Agent
/// footprint", §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FootprintReport {
    /// [`FOOTPRINT_SCHEMA_VERSION`].
    pub schema_version: u32,
    pub ddc_version: String,
    /// RFC 3339, UTC.
    pub generated_at: String,
    pub duration_ms: u64,
    /// The folder asked about, canonical.
    #[serde(serialize_with = "crate::json_path::path")]
    pub root: PathBuf,
    /// The main checkout, or the bare repository.
    #[serde(serialize_with = "crate::json_path::path")]
    pub main: PathBuf,
    /// `main` is a bare repository: it has no checkout of its own and is not walked.
    pub bare: bool,
    /// Why `git worktree list` gave no answer; absent when it did. Without
    /// it only the main checkout and what lies inside it are reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing_error: Option<String>,
    /// The main checkout first (unless bare), then each worktree, largest first.
    pub checkouts: Vec<CheckoutEntry>,
    /// Worktrees git lists that the walk did not read.
    pub not_scanned: Vec<NotScanned>,
    pub totals: FootprintTotals,
    /// The folders `ddc clean` would review to remove what is selected: the
    /// main checkout only, never a parent of a worktree outside it.
    #[serde(serialize_with = "crate::json_path::paths")]
    pub clean_roots: Vec<PathBuf>,
    /// Selected findings outside the main checkout, which `ddc clean <main>`
    /// does not reach: the app's Review window lists them.
    #[serde(serialize_with = "crate::json_path::paths")]
    pub outside_main: Vec<PathBuf>,
    /// The coding agents' data of the last home scan; absent with `harness_reason`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<HarnessPart>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness_reason: Option<HarnessReason>,
    pub min_age_days: u64,
    pub worktree_min_age_days: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CheckoutKind {
    Main,
    /// A linked worktree.
    Linked,
    /// A `.git` file into a gitdir that is gone (`git.worktree-orphan`).
    Orphan,
}

/// One checkout: its state and the findings inside it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CheckoutEntry {
    #[serde(serialize_with = "crate::json_path::path")]
    pub path: PathBuf,
    pub kind: CheckoutKind,
    /// Who made it; absent for the main checkout and for a worktree kept by
    /// an exclude or a `.ddc-keep` marker (a claim, not a worktree region).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// The six conditions' facts, as `ddc scan` reports them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeState>,
    /// Its `git worktree lock`, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<Lock>,
    /// Where it counts; absent for the main checkout, which counts in no bucket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket: Option<Bucket>,
    /// Why it is held back; with `bucket: held-back` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<HeldReason>,
    /// The checkout's own bytes and every finding inside it; for the main
    /// checkout, its findings only (not the size of its files).
    pub bytes: u64,
    /// Why, in words, as `ddc scan` says it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hint: String,
    /// The findings inside it, not in a nested checkout; the checkout's own
    /// `git.worktree` finding is not among them.
    pub findings: Vec<Finding>,
}

/// A `git worktree lock` and whether its holder runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Lock {
    /// The pid its reason names (`(pid N`); absent when it names none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Whether that pid runs as the current user: an estimate, since the
    /// pid may have been reused. Absent without a pid, or without a process
    /// table on this platform; then a lock with a pid is taken as live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Bucket {
    /// Selected now under the six conditions.
    Now,
    /// It fails only condition 2 (a live lock, or in use) or 3 (used within
    /// the window), and its probes found it otherwise removable: an estimate
    /// that assumes it stays as it is.
    AfterSessions,
    /// Anything else, with its reason.
    HeldBack,
}

/// Why a checkout is held back: the first condition that fails, or why it
/// is not judged at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum HeldReason {
    Submodules,
    Dirty,
    IgnoredContent,
    OwnCommits,
    /// Locked, and its lock names no pid or one that does not run.
    StaleLock,
    ProbeUnknown,
    /// Copied or moved by hand: git does not know it there.
    NotLinked,
    /// Made by a harness whose worktrees ddc does not select.
    NotAHarness,
    /// Its gitdir is gone.
    Orphan,
    /// The person's own worktree.
    Yours,
    /// A file inside it is immutable.
    Immutable,
    /// Not the current user's.
    ForeignOwner,
    /// Under an exclude, or holding `.ddc-keep`.
    Kept,
    /// Under `min_size` in `settings.json`.
    HiddenBySettings,
    /// Nothing tells when it was last used.
    ActivityUnknown,
    /// `git.worktree` is switched off in `settings.json`.
    RuleDisabled,
}

/// A worktree git lists that the walk did not read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NotScanned {
    #[serde(serialize_with = "crate::json_path::path")]
    pub path: PathBuf,
    pub reason: NotScannedReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum NotScannedReason {
    /// Under a skip of `settings.json` or the command line.
    Skipped,
    /// git lists it and the folder is gone, or does not resolve.
    Missing,
    /// On another filesystem than its parent.
    OtherFilesystem,
    /// Read, but not recognised as a worktree.
    NotRecognized,
}

/// Bytes by class, and what can go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct FootprintTotals {
    /// Every finding inside the checkouts, by class; each byte once.
    pub safe: Amount,
    pub rebuildable: Amount,
    pub review: Amount,
    pub user_data: Amount,
    /// Selected now: `safe` items (an agent may clean them once the person's
    /// opt-in is on) apart from `rebuildable` ones (the person's), a selected
    /// worktree with everything inside it.
    pub reclaimable_now: Reclaimable,
    /// The worktrees of `bucket: after-sessions`, without what inside them
    /// is selected now (counted in `reclaimable_now`): an estimate.
    pub reclaimable_after_sessions: u64,
    /// The worktrees of `bucket: held-back`, without what inside them is
    /// selected now. No byte is in two of these sums.
    pub held_back: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Amount {
    pub count: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Reclaimable {
    pub safe: u64,
    pub rebuildable: u64,
    pub bytes: u64,
}

/// What the last home scan found of the coding agents' own data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HarnessPart {
    /// Seconds since that scan was written.
    pub report_age: u64,
    /// Its `agent.*` findings.
    pub findings: Vec<Finding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum HarnessReason {
    /// No `last-scan.json` in the data directory.
    NoReport,
    /// It was written by another report schema.
    SchemaMismatch,
    /// It could not be read or decoded.
    Unreadable,
}
