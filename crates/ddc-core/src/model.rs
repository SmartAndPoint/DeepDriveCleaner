//! The scan report: the JSON contract between the core, the CLI, the
//! background agent and the future Mac app. New optional fields keep
//! [`SCHEMA_VERSION`]; renames, removals, type changes and new enum variants
//! bump it (strict decoders such as Swift `Codable` reject unknown variants).

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::class::{Category, Removal, RiskClass};
use crate::platform::{Capability, Platform};
use crate::rules::Note;
use crate::rules::entries::EntryLock;
use crate::scan::SkipReason;

/// 2: `platform` and `capabilities`, against which the other fields read
/// (ADR "Platform seam: one platform module per OS with declared
/// capabilities", §4). 3: the entry locks `live-session` and
/// `liveness-unknown`, on the entries of the coding agents' session folders
/// (ADR "Agent footprint: worktrees, harness data and bounded consent for
/// agents", §3).
pub const SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ScanReport {
    pub schema_version: u32,
    pub ddc_version: String,
    /// The platform the CLI was built for.
    pub platform: Platform,
    /// What this build can do on its platform, in declaration order. A
    /// capability not listed is absent: without `private-size` no finding
    /// has `private_bytes`; without `full-disk-access` `full_disk_access`
    /// is true.
    pub capabilities: Vec<Capability>,
    /// RFC 3339, UTC.
    pub generated_at: String,
    pub duration_ms: u64,
    pub home: PathBuf,
    pub scan_roots: Vec<PathBuf>,
    /// True unless a permission system withheld protected data. On macOS,
    /// whether the scanning process could read TCC-protected data: without
    /// it parts of `~/Library` are unreadable and listed in `inaccessible`.
    pub full_disk_access: bool,
    /// The volume of each scan root, once per volume. Absent when no volume
    /// could be read.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<Volume>,
    /// `rebuildable` findings idle at least this long are selected by default.
    pub min_age_days: u64,
    pub totals: Totals,
    /// Ordered by class, then size (largest first), then path.
    pub findings: Vec<Finding>,
    /// Largest files outside every finding, for manual review.
    pub large_files: Vec<LargeFile>,
    /// Directories that could not be read (at most 10 000; see `errors_total`).
    pub inaccessible: Vec<Inaccessible>,
    pub errors_total: u64,
    pub skipped: Vec<SkippedPath>,
    pub skipped_total: u64,
    pub cancelled: bool,
    /// The user's skips (`settings.json`'s `skips`, `--skip`) the walk stopped
    /// at, canonical, never truncated. A skip inside a claimed finding is not
    /// here: the finding was read and is kept whole instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub not_scanned: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Totals {
    /// Allocated bytes under the scan roots, each inode once.
    pub scanned_bytes: u64,
    pub files: u64,
    pub dirs: u64,
    /// Bytes outside every finding.
    pub unowned_bytes: u64,
    /// One entry per class, in the order safe, rebuildable, review, user-data.
    pub classes: Vec<ClassTotal>,
    /// Bytes of findings selected by default.
    pub selected_bytes: u64,
    /// Estimate of what deleting the selected findings frees: APFS private
    /// size where clones are expected and measured (uv, pnpm), allocated size
    /// elsewhere (no clones expected there).
    pub selected_reclaimable_bytes: u64,
    /// Findings `settings.json` hides ([`Finding::ignored`]); they are in no
    /// class total above.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ignored_count: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub ignored_bytes: u64,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Volume {
    pub mount_point: PathBuf,
    /// The scan roots on this volume; the walk never crosses into another,
    /// so every finding under them lies on it.
    pub roots: Vec<PathBuf>,
    /// Bytes available to an unprivileged user.
    pub free_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClassTotal {
    pub class: RiskClass,
    pub count: u64,
    pub bytes: u64,
    pub selected_count: u64,
    pub selected_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RemovalKind {
    Delete,
    Trash,
    Native,
    NativeOnly,
    None,
}

impl From<&Removal> for RemovalKind {
    /// The rule's own method; a finding downgraded to review may differ (see `removal`).
    fn from(removal: &Removal) -> Self {
        match removal {
            Removal::Delete => RemovalKind::Delete,
            Removal::Trash => RemovalKind::Trash,
            Removal::Native { .. } => RemovalKind::Native,
            Removal::NativeOnly(_) => RemovalKind::NativeOnly,
            Removal::None => RemovalKind::None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Finding {
    pub path: PathBuf,
    /// Stable rule id (`python.venv`, `cache.uv`, …).
    pub rule: String,
    pub category: Category,
    /// Effective class (may be lower than `rule_class`, see `notes`).
    pub class: RiskClass,
    pub rule_class: RiskClass,
    pub removal: RemovalKind,
    /// Allocated bytes belonging to this finding alone.
    pub bytes: u64,
    /// Allocated bytes of hard-linked files also linked elsewhere.
    pub shared_bytes: u64,
    /// Lower bound of what deleting it frees (APFS private size). Present
    /// where clones are expected (uv, pnpm) and measurable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_bytes: Option<u64>,
    pub files: u64,
    pub dirs: u64,
    /// Some content could not be read or was skipped; sizes are lower bounds.
    pub incomplete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<Project>,
    /// Selected by default in a cleanup plan.
    pub selected: bool,
    /// Why it is not selected by default. An ignored finding keeps the reason
    /// it would have had, so a type switched back on applies at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_selected: Option<NotSelected>,
    /// Hidden by `settings.json`: its rule is switched off, or it is under
    /// `min_size`. Never selected and in no total; `not_selected` is unchanged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ignored: bool,
    pub notes: Vec<Note>,
    /// How to get it back.
    pub hint: String,
    pub dev: u64,
    pub ino: u64,
    /// Identity of the parent directory at scan time (re-checked before cleaning).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_dev: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_ino: Option<u64>,
    /// What one of the user's own folders holds, to pick from (story "User
    /// data: pick your own files for the Trash"), or the sessions of a
    /// coding agent's session folder (story "Harness data rules"). Only on
    /// findings of a rule with an entry policy (`rules::entries::policy`)
    /// the scan could read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contents: Option<Contents>,
    /// What keeps a `user.kept` finding (story "Protected by you: say what it
    /// is, and Remove Protection"). Only on those findings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protection: Option<Protection>,
    /// The newest modification time inside it, its own directory included,
    /// RFC 3339 in UTC to the second (story "Review window: sort a group's
    /// rows by name, size or last change"). A lower bound when `incomplete`.
    /// Only inside a claim the walk read times for: none on a region, a
    /// user-data finding, a protected one, an unlisted rule's, or one whose
    /// root could not be opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    /// The harness whose own structure the rule recognizes (ADR "Agent
    /// footprint: worktrees, harness data and bounded consent for agents",
    /// §1). Only on findings of such rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
}

/// Where a finding comes from: a coding agent's harness and what of it the
/// finding is. A provenance, not a class: the class and the method decide
/// what happens to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct Origin {
    pub harness: OriginHarness,
    pub kind: OriginKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum OriginHarness {
    ClaudeCode,
    Codex,
    Opencode,
    Cursor,
    /// A git worktree, whoever made it.
    Git,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum OriginKind {
    /// A worktree an agent works in.
    Worktree,
    /// What the harness keeps of its sessions: transcripts, history, logs.
    SessionData,
    /// What the harness downloads or writes for itself and can make again.
    RuntimeCache,
}

/// What protects a `user.kept` finding: its keep markers, the exclusions and
/// skips at or above it or inside it, and the rule it displaced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Protection {
    /// Each folder holding a `.ddc-keep`, the finding's own or one inside it,
    /// sorted by folder.
    pub markers: Vec<MarkerFolder>,
    /// Sorted by path, each once.
    pub exclusions: Vec<Exclusion>,
    /// The rule that would have claimed the folder, when the walk knows it.
    /// Under an exclusion at or above the folder nothing is listed, so only a
    /// known path, a region or a child of a known path is known there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
}

/// A folder holding a `.ddc-keep`, with its identity at scan time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MarkerFolder {
    pub folder: PathBuf,
    pub dev: u64,
    pub ino: u64,
}

/// An exclusion or a skip that protects a finding, canonical.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Exclusion {
    pub path: PathBuf,
    pub kind: ExclusionKind,
    pub origin: ExclusionOrigin,
    /// The path lies inside the finding; otherwise it is the finding's own
    /// folder or above it.
    pub inside: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ExclusionKind {
    /// Never touched (`excludes`, `--exclude`).
    Exclude,
    /// Never read (`skips`, `--skip`).
    Skip,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ExclusionOrigin {
    /// `settings.json`.
    #[default]
    Settings,
    /// A command-line flag.
    Flag,
}

/// The direct children of a folder with entries: the largest listed, the
/// rest summed. A session folder's entry is a session: its transcript (or
/// its folder in `file-history`) with the siblings named after it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Contents {
    /// Largest first (ties by path bytes); at most 100. A user-data folder's
    /// are each at least 10 MiB; a session folder's have no floor.
    pub entries: Vec<FolderEntry>,
    /// The other children, findings of their own left out.
    pub rest_count: u64,
    pub rest_bytes: u64,
}

/// One direct child of a folder with entries: a regular file or a
/// directory. A session's entry is its key (`<session>.jsonl`, or
/// `<session>/` in `file-history`), with its companions' bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FolderEntry {
    pub path: PathBuf,
    pub kind: FolderEntryKind,
    /// Allocated bytes, findings inside it included; a session's, its
    /// companions' too.
    pub bytes: u64,
    pub dev: u64,
    pub ino: u64,
    /// Last modification, RFC 3339; a session's, the newest of its files'
    /// own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    /// Why it cannot be picked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lock: Option<EntryLock>,
    /// Parts of it could not be read; `bytes` is a lower bound. It can still
    /// be picked: the Trash keeps it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub incomplete: bool,
    /// Findings of other rules inside it (a folder in Downloads).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub findings: u64,
    /// A folder with `.git`: a project.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub git: bool,
    /// A device backup's `Info.plist`, where it could be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup: Option<Backup>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum FolderEntryKind {
    File,
    Folder,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Backup {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    /// RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_backup: Option<String>,
}

impl Finding {
    /// Freed bytes: the private size where measured (a lower bound), else
    /// the allocated size (exact when nothing is cloned).
    pub fn reclaimable_bytes(&self) -> u64 {
        self.private_bytes.unwrap_or(self.bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Project {
    pub root: PathBuf,
    /// Latest activity (RFC 3339): git index/HEAD, manifests, lock files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_days: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_lockfile: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum NotSelected {
    /// Expensive to restore or restorability uncertain: always a manual choice.
    Review,
    /// The user's data: never planned.
    UserData,
    /// The project changed recently.
    ActiveProject { idle_days: u64 },
    /// No activity signal: treated as active.
    UnknownActivity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LargeFile {
    pub path: PathBuf,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Inaccessible {
    pub path: PathBuf,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SkippedPath {
    pub path: PathBuf,
    pub reason: SkipReason,
}
