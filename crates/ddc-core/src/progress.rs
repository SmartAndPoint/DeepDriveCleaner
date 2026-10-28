//! Progress events on stderr for `--progress json` (story "CLI: progress
//! events"; ADR "Menu bar app: Swift shell over the ddc CLI", §6 item 3).
//! Each line is one [`ProgressLine`]; other lines on the stream (`ddc:`
//! warnings, the signal's notice) are not events. Schema:
//! `docs/progress.schema.json`.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::exec::{ExecSnapshot, Outcome};
use crate::plan::{ActionKind, PlanItem};
use crate::scan::ProgressSnapshot;

/// Version of [`ProgressLine`]; the reports have their own.
pub const PROGRESS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProgressLine {
    pub schema_version: u32,
    #[serde(flatten)]
    pub event: ProgressEvent,
}

impl ProgressLine {
    pub fn new(event: ProgressEvent) -> Self {
        Self { schema_version: PROGRESS_SCHEMA_VERSION, event }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "event", rename_all = "kebab-case")]
pub enum ProgressEvent {
    /// A phase begins; never held back.
    Phase(PhaseEvent),
    /// A walk's counters: at most one every 100 ms, plus each step's final one.
    Walk(WalkEvent),
    /// The clean's item counters: at most one every 100 ms, plus a final one.
    Items(ItemsEvent),
    /// One item's outcome, after its journal record; never held back.
    Item(ItemEvent),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PhaseEvent {
    pub phase: Phase,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    /// After the lock, before the clean's re-check walk.
    Preparing,
    /// Immediately before the first action: from here on the clean may touch
    /// files. Written even when no item runs.
    Cleaning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WalkEvent {
    pub step: WalkStep,
    pub files: u64,
    pub dirs: u64,
    /// Allocated bytes so far; a hard-linked file counts once per link until
    /// the final tick, which carries the report's totals.
    pub bytes: u64,
    /// An estimate: for `recheck`, the previous report's files when the
    /// whole tree is walked, and its listed findings' files (with the
    /// siblings they are judged by) when the walk is pruned to the list;
    /// for `markers`, the executed items' files. Nested items count twice, a
    /// region counts less, a picked entry counts none. Absent for `scan`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_expected: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum WalkStep {
    /// `ddc scan`.
    Scan,
    /// The clean's re-check of the disk before its plan.
    Recheck,
    /// The clean's walk of the chosen items for keep markers.
    Markers,
    /// `ddc disk`: the whole disk, size only.
    Disk,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ItemsEvent {
    /// Items with an outcome; `not-run` items are not counted.
    pub done: u64,
    /// Items the executor runs; items refused at the re-check are only in the report.
    pub n: u64,
    /// Bytes of the items done, whatever their outcome; `native-only` items
    /// are in neither byte figure (their space is freed inside a VM image).
    pub bytes_done: u64,
    pub bytes_total: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<CurrentEvent>,
}

/// The item in flight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CurrentEvent {
    /// 1-based, of `n`.
    pub i: u64,
    pub path: PathBuf,
    pub action: ActionKind,
    pub bytes: u64,
    /// The tool whose command (or location probe) runs now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ItemEvent {
    /// 1-based, of `n`.
    pub i: u64,
    pub n: u64,
    pub path: PathBuf,
    pub action: ActionKind,
    /// Allocated bytes as the clean's re-check measured them.
    pub bytes: u64,
    #[serde(flatten)]
    pub outcome: Outcome,
}

impl WalkEvent {
    pub fn new(step: WalkStep, snap: ProgressSnapshot, files_expected: Option<u64>) -> Self {
        Self { step, files: snap.files, dirs: snap.dirs, bytes: snap.bytes, files_expected }
    }
}

impl From<&ExecSnapshot> for ItemsEvent {
    fn from(snap: &ExecSnapshot) -> Self {
        Self {
            done: snap.items_done,
            n: snap.items_total,
            bytes_done: snap.bytes_done,
            bytes_total: snap.bytes_total,
            current: snap.current.as_ref().map(|c| CurrentEvent {
                i: c.index,
                path: c.path.clone(),
                action: c.action,
                bytes: c.bytes,
                tool: c.tool.map(str::to_owned),
            }),
        }
    }
}

impl ItemEvent {
    pub fn new(i: u64, n: u64, item: &PlanItem, outcome: &Outcome) -> Self {
        Self {
            i,
            n,
            path: item.finding.path.clone(),
            action: item.action.kind(),
            bytes: item.finding.bytes,
            outcome: outcome.clone(),
        }
    }
}
