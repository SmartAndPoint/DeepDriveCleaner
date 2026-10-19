//! The background scan's notify decision as JSON, for the app to show the
//! notification itself (`scan --background --notify json`; ADR "Menu bar app:
//! Swift shell over the ddc CLI", §5 and §6 item 5). The rule lives in the CLI;
//! this is its contract. Schema: `docs/notify.schema.json`.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Version of [`NotifyReport`]; the scan report has its own.
pub const NOTIFY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NotifyReport {
    pub schema_version: u32,
    #[serde(flatten)]
    pub status: NotifyStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum NotifyStatus {
    /// The scan ran and `last-scan.json` holds its report.
    Scanned(Decision),
    /// Nothing was scanned.
    Skipped { reason: SkipCause },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SkipCause {
    /// A clean held the lock.
    Busy,
}

/// The optional fields are always written, as null when absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Decision {
    pub notify: bool,
    /// Null exactly when `notify` is false; `low-disk` wins when both apply.
    pub reason: Option<NotifyReason>,
    /// The volume the decision is about: the one under 10 % free with
    /// something selected on it, also when the snooze or the repeat rule holds
    /// the notification back; otherwise the first scan root's.
    pub volume: Option<PathBuf>,
    /// What the default selection frees on all volumes together (estimate).
    pub reclaimable: u64,
    pub free: Option<u64>,
    pub total: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum NotifyReason {
    /// At least the threshold can be freed.
    Threshold,
    /// A volume is under 10 % free and something on it can be freed.
    LowDisk,
}
