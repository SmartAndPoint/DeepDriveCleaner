//! `ddc changes`: what grew between the last two scans (story "ddc changes:
//! what grew between the last two scans", DDC-001; ADR "Menu bar app: Swift
//! shell over the ddc CLI", §6 item 15). It compares `previous-scan.json`
//! with `last-scan.json`, the pair the story "Keep the previous scan report,
//! and say who wrote each" keeps, and reads the action journal for the
//! person's own cleans. It walks nothing, takes no lock and writes nothing.
//! Schema: `docs/changes.schema.json`.

mod compare;
mod journal;
mod load;
mod not_read;

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::{OriginHarness, WrittenBy};

pub use compare::{compare, compare_loaded};
pub use journal::{JournalLine, parse_journal, read_journal};
pub use load::{Loaded, load, load_previous};
pub use not_read::{cloud_roots, not_read};

/// Version of [`ChangesReport`]; the scan report has its own.
pub const CHANGES_SCHEMA_VERSION: u32 = 1;

/// What `ddc changes --json` prints. Its own optional fields are always
/// written, as null when absent; a row's (`trashed_in`, `moved_from`,
/// `harness`, `approximate`) are left out when absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChangesReport {
    pub schema_version: u32,
    pub status: Status,
    /// Why the two reports were not compared; null when they were.
    pub reason: Option<Reason>,
    /// `previous-scan.json`, when it decoded.
    pub baseline: Option<Side>,
    /// `last-scan.json`, when it decoded.
    pub current: Option<Side>,
    /// The roots both reports read: the reports' own when equal, else the
    /// inner of two nested roots. Empty when not compared.
    #[serde(serialize_with = "crate::json_path::paths")]
    pub compared_roots: Vec<PathBuf>,
    /// In-use space on the home folder's volume. Null when not compared, or
    /// when either report lacks that volume.
    pub disk: Option<Disk>,
    pub home: Option<Home>,
    pub rest: Option<Rest>,
    /// The person's own cleans between the two saves that named a finding of
    /// the baseline, from the action journal.
    pub cleaned_since: Option<Cleaned>,
    /// One entry per Review group with a compared change, biggest growth
    /// first.
    pub groups: Vec<Group>,
    /// Every finding that changed, biggest change first.
    pub items: Vec<Item>,
    /// In-use space on the home folder's volume grew by at least
    /// max(20 GiB, 5 % of its total) between the two reports: the rule of
    /// `ddc space`'s `grew` (ADR "Menu bar app: Swift shell over the ddc
    /// CLI" §6 item 13). False when not compared or without `disk`.
    #[serde(default)]
    pub grew: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Compared,
    NotCompared,
}

/// Why two reports were not compared. When several hold, the first in this
/// order is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Reason {
    /// No `last-scan.json`.
    NoReport,
    /// No `previous-scan.json`.
    NoPrevious,
    /// A report that does not decode with this version's types.
    OtherSchema,
    /// A report without `written_at` or `rules_in_force`: saved before
    /// version 0.31.
    PredatesFields,
    /// A report of a cancelled scan.
    Cancelled,
    /// No root of one report lies at or inside a root of the other.
    NoSharedRoot,
    /// `last-scan.json` does not name `previous-scan.json` as its pair.
    OutOfStep,
}

/// One report, as the output names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Side {
    pub written_at: Option<String>,
    pub written_by: Option<WrittenBy>,
    pub generated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Disk {
    #[serde(serialize_with = "crate::json_path::path")]
    pub mount_point: PathBuf,
    /// Total minus free bytes in the baseline.
    pub before_in_use: u64,
    pub now_in_use: u64,
    /// `now_in_use - before_in_use`: positive when in-use space grew.
    pub delta: i64,
    /// RFC 3339: when each report read the free space, the volume's
    /// `read_at`, else the report's `generated_at`.
    pub before_at: String,
    pub now_at: String,
}

/// The scanned folders as a whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Home {
    /// Both reports read the same folders the same way, so their totals
    /// compare.
    pub compared: bool,
    /// Why not, when `compared` is false.
    pub reasons: Vec<HomeReason>,
    /// `totals.scanned_bytes` now minus before; null when not compared.
    pub delta: Option<i64>,
    /// The change of the ignored findings (types switched off), which the
    /// groups leave out.
    pub ignored_delta: i64,
    /// What `delta` leaves after the compared findings: growth in folders
    /// that are no finding, and findings not compared. Null when not
    /// compared.
    pub elsewhere: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum HomeReason {
    DifferentHome,
    DifferentRoots,
    /// One scan had Full Disk Access and the other not.
    DifferentAccess,
    /// A report's `skipped` or `inaccessible` list was cut short.
    CutList,
    /// The reports did not read the same folders, `changed` ones apart.
    DifferentNotRead,
}

/// What the disk's change leaves after the scanned folders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Rest {
    /// The disk's `delta` minus the home's `delta` (or, when the home does
    /// not compare, minus the compared findings' change). Null without a
    /// disk.
    pub delta: Option<i64>,
    /// What the current report did not read.
    pub not_read: Vec<NotRead>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NotRead {
    #[serde(serialize_with = "crate::json_path::path")]
    pub path: PathBuf,
    pub kind: NotReadKind,
}

/// Why a folder was not read, which picks its label and action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum NotReadKind {
    /// A cloud folder, never read.
    Cloud,
    /// A folder the person skips (`skips` in `settings.json`, `--skip`).
    Skip,
    /// A folder macOS asks before reading, left out of a scan without Full
    /// Disk Access.
    Prompting,
    /// A folder the system refused to list.
    Refused,
    /// A folder whose files are in the cloud only.
    Evicted,
    /// Another disk mounted inside a root.
    OtherDisk,
    /// A file system snapshot (Linux).
    Snapshot,
    /// A folder that changed while the walk listed it.
    Changed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Cleaned {
    pub deleted: u64,
    pub trashed: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Group {
    /// The Review group's label, as `ddc rules --json` gives it; "Other" for
    /// a rule in no group.
    pub group: String,
    /// The net change, the person's cleans and the Trash's intake from them
    /// left out.
    pub delta: i64,
    pub new: u64,
    pub grew: u64,
    pub shrank: u64,
    pub gone: u64,
    pub cleaned: u64,
    pub moved: u64,
    /// The part in each coding agent's worktrees or data.
    pub by_harness: Vec<HarnessDelta>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HarnessDelta {
    pub harness: OriginHarness,
    pub delta: i64,
    pub count: u64,
}

/// One finding that changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Item {
    #[serde(serialize_with = "crate::json_path::path")]
    pub path: PathBuf,
    pub rule: String,
    pub kind: ItemKind,
    /// Allocated bytes in each report; null on the side without it.
    pub before: Option<u64>,
    pub now: Option<u64>,
    /// `now - before`, a missing side counting as 0.
    pub delta: i64,
    /// The bytes of it the person's cleans removed.
    pub cleaned: u64,
    /// For the Trash: the growth that is the person's trashed items.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trashed_in: Option<u64>,
    /// For a `moved` row: the path in the baseline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(serialize_with = "crate::json_path::opt_path")]
    pub moved_from: Option<PathBuf>,
    /// The coding agent whose worktree or data it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<OriginHarness>,
    /// Ignored in the current report (or the baseline, when gone).
    pub ignored: bool,
    /// Either side shares some of its bytes with other files (clones, hard
    /// links): its size is an upper bound.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub approximate: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ItemKind {
    New,
    Grew,
    Shrank,
    Gone,
    /// Gone or shrunk by the person's own cleans, wholly.
    Cleaned,
    /// Gone at one path and new at another with the same identity and rule.
    Moved,
    /// One report did not read where it lies, or lacked its rule.
    NotCompared,
}

impl ChangesReport {
    /// The output for two reports that were not compared.
    pub fn not_compared(reason: Reason, baseline: Option<Side>, current: Option<Side>) -> Self {
        Self {
            schema_version: CHANGES_SCHEMA_VERSION,
            status: Status::NotCompared,
            reason: Some(reason),
            baseline,
            current,
            compared_roots: Vec::new(),
            disk: None,
            home: None,
            rest: None,
            cleaned_since: None,
            groups: Vec::new(),
            items: Vec::new(),
            grew: false,
        }
    }
}

impl Side {
    pub fn of(report: &crate::model::ScanReport) -> Self {
        Self {
            written_at: report.written_at.clone(),
            written_by: report.written_by,
            generated_at: report.generated_at.clone(),
        }
    }
}
