//! The scan report: the JSON contract between the core, the CLI, the
//! background agent and the future Mac app. New optional fields keep
//! [`SCHEMA_VERSION`]; renames, removals, type changes and new enum variants
//! bump it (strict decoders such as Swift `Codable` reject unknown variants).

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::class::{Category, Removal, RiskClass};
use crate::rules::Note;
use crate::scan::SkipReason;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ScanReport {
    pub schema_version: u32,
    pub ddc_version: String,
    /// RFC 3339, UTC.
    pub generated_at: String,
    pub duration_ms: u64,
    pub home: PathBuf,
    pub scan_roots: Vec<PathBuf>,
    /// Whether the scanning process could read TCC-protected data. Without it
    /// parts of `~/Library` are unreadable and listed in `inaccessible`.
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
