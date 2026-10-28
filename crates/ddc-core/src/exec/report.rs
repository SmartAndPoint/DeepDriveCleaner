//! The report of `clean --items --json` (ADR "Menu bar app: Swift shell over
//! the ddc CLI", §6 item 1): every listed item's outcome in list order, the
//! free space of each volume touched, the journal. Schema: `docs/clean.schema.json`.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{ExecReport, Outcome, VolumeSpace};
use crate::class::RiskClass;
use crate::plan::{ActionKind, ListedItem, Refused};

/// Version of [`CleanReport`]; the scan report has its own. 2: the skip
/// reasons `not-an-entry`, `entry-locked` and `inside-another-item`. 3: the
/// skip reason `tool-not-ready`.
pub const CLEAN_SCHEMA_VERSION: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CleanReport {
    pub schema_version: u32,
    pub ddc_version: String,
    /// A signal stopped the run; the items it left are `not-run`.
    pub cancelled: bool,
    /// In the order of the list.
    pub items: Vec<CleanItem>,
    /// Items that `failed`, or whose tool command failed.
    pub failed: u64,
    pub volumes: Vec<VolumeSpace>,
    /// Local Time Machine snapshots keep deleted blocks until macOS thins them.
    pub local_snapshots: usize,
    /// The action journal every started item was recorded in.
    pub journal: PathBuf,
    /// `last-scan.json` was edited by what this clean did and saved, so no
    /// scan needs to follow (story "Faster clean: re-check only what the
    /// selection needs"). Absent from an older CLI: read as false.
    #[serde(default)]
    pub last_scan_saved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CleanItem {
    pub path: PathBuf,
    pub dev: u64,
    pub ino: u64,
    pub rule: String,
    pub class: RiskClass,
    pub action: ActionKind,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub permanent: bool,
    /// Allocated bytes the fresh scan measured; absent when the item was
    /// refused before it was run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(flatten)]
    pub outcome: Outcome,
}

impl CleanReport {
    pub fn new(listed: &[ListedItem], refused: &[Refused], run: &ExecReport, journal: PathBuf) -> Self {
        let items: Vec<CleanItem> = listed
            .iter()
            .map(|item| {
                let (bytes, outcome) = match refused.iter().find(|r| r.item.path == item.path) {
                    Some(r) => (None, Outcome::Skipped(r.reason.clone())),
                    None => match run.items.iter().find(|i| i.path == item.path) {
                        Some(done) => (Some(done.bytes), done.outcome.clone()),
                        // Every confirmed item is executed; this only guards the invariant.
                        None => (None, Outcome::NotRun),
                    },
                };
                CleanItem {
                    path: item.path.clone(),
                    dev: item.dev,
                    ino: item.ino,
                    rule: item.rule.clone(),
                    class: item.class,
                    action: item.action,
                    permanent: item.permanent,
                    bytes,
                    outcome,
                }
            })
            .collect();
        let failed = items
            .iter()
            .filter(|i| match &i.outcome {
                Outcome::Failed { .. } => true,
                Outcome::Native { commands } => commands.iter().any(|c| !c.success),
                _ => false,
            })
            .count() as u64;
        Self {
            schema_version: CLEAN_SCHEMA_VERSION,
            ddc_version: env!("CARGO_PKG_VERSION").to_owned(),
            cancelled: run.cancelled,
            items,
            failed,
            volumes: run.volumes.clone(),
            local_snapshots: run.local_snapshots,
            journal,
            last_scan_saved: false,
        }
    }
}
