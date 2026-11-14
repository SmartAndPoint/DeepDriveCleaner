//! Action journal: one JSON line per item in `<log-dir>/actions.jsonl`, one
//! per area action (safety ADR, amendment of 2026-10-28), and one per
//! decision of `ddc keep remove` (amendment of 2026-10-30).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::PathBuf;

use serde::Serialize;

use super::{MovedCompanion, Outcome, TrashPlace};
use crate::class::RiskClass;
use crate::disk::ActionId;
use crate::env::Env;
use crate::free::ActionResult;
use crate::keep::{KeepReport, KeepResult};
use crate::plan::PlanItem;

/// The journal's own version. Its lines carried the scan report's
/// `SCHEMA_VERSION` (1) until area actions came, so it starts at 2; a
/// version 1 line has no `kind` and is an item line. 3: the skip reason
/// `tool-not-ready`. 4: the kind `keep-remove`. 5: in an `item` line, the
/// skip reason `agent-live` and the entry locks `live-session` and
/// `liveness-unknown`, and the optional `entry` (ADR "Agent footprint:
/// worktrees, harness data and bounded consent for agents", §3). 6: in an
/// `item` line, the skip reason `foreign-owner` (safety ADR, amendment of
/// 2026-10-31). 7: in an `item` line, the skip reason `worktree-kept` (ADR
/// "Agent footprint", §2, amendment of 2026-11-01).
/// 8: in an `item` line, the skip reason `trash-crosses-filesystem` and the
/// optional `trash`, where the item went (safety ADR §5, amendment of
/// 2026-11-01). 9: in an `item` line, the entry locks `build-in-use` and
/// `builds-unknown` (safety ADR, amendment of 2026-11-02). 10: in an `item`
/// line, the skip reason `held-open` (safety ADR §5, amendment of
/// 2026-11-02). 11: in an `item` line, the skip reason `recycle-bin-refuses`
/// with its `cause`, and the outcome `removed-not-recycled`, a session's
/// companion's line included (safety ADR §5, the Recycle Bin on Windows,
/// planned 2026-11-04). 12: in an `item` line, the skip reason `immutable`
/// and the entry lock `immutable` (safety ADR, amendment of 2026-11-04,
/// "Linux file kinds"). 13: the skip reasons `not-consented` and
/// `agent-opt-in-off`, and an `item` line's optional `consent` and
/// `project` (ADR "Agent footprint", §7, story "Clean: --agent bounded
/// consent").
pub const JOURNAL_SCHEMA_VERSION: u32 = 13;

/// Where an item went in the Trash: the trash folder and the name in its
/// `files/`. Absent where the Trash does not tell (macOS): a trash on
/// another filesystem is not shown by every desktop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrashedTo {
    #[serde(serialize_with = "crate::json_path::path")]
    pub trash: PathBuf,
    #[serde(serialize_with = "crate::json_path::path")]
    pub name: PathBuf,
}

impl From<TrashPlace> for TrashedTo {
    fn from(p: TrashPlace) -> Self {
        Self { trash: p.trash, name: PathBuf::from(p.name) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum JournalKind {
    Item,
    AreaAction,
    /// A decision of `ddc keep remove` under the lock.
    KeepRemove,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JournalEntry {
    pub schema_version: u32,
    pub kind: JournalKind,
    pub ts: String,
    #[serde(serialize_with = "crate::json_path::path")]
    pub path: PathBuf,
    pub dev: u64,
    pub ino: u64,
    pub rule: String,
    pub class: RiskClass,
    pub method: &'static str,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_bytes: Option<u64>,
    /// On a session's companion, moved with its entry: the entry's
    /// transcript, the path the item was listed by. Each moved path has a
    /// line of its own, so each can be put back; absent on every other line.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(serialize_with = "crate::json_path::opt_path")]
    pub entry: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trash: Option<TrashedTo>,
    /// Who consented; set on the lines of `ddc agent clean`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consent: Option<super::ConsentKind>,
    /// The main checkout an agent's clean was bound to.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(serialize_with = "crate::json_path::opt_path")]
    pub project: Option<PathBuf>,
    #[serde(flatten)]
    pub outcome: Outcome,
}

impl JournalEntry {
    pub fn new(item: &PlanItem, outcome: &Outcome) -> Self {
        let f = &item.finding;
        Self {
            schema_version: JOURNAL_SCHEMA_VERSION,
            kind: JournalKind::Item,
            ts: now(),
            path: f.path.clone(),
            dev: f.dev,
            ino: f.ino,
            rule: f.rule.clone(),
            class: f.class,
            method: item.action.name(),
            bytes: f.bytes,
            private_bytes: f.private_bytes,
            entry: None,
            trash: None,
            consent: None,
            project: None,
            outcome: outcome.clone(),
        }
    }

    /// A companion of `item`, a session's entry, moved to the Trash with it,
    /// or destroyed by the shell on its way there.
    pub fn companion(item: &PlanItem, moved: &MovedCompanion) -> Self {
        let f = &item.finding;
        Self {
            schema_version: JOURNAL_SCHEMA_VERSION,
            kind: JournalKind::Item,
            ts: now(),
            path: moved.path.clone(),
            dev: moved.id.0,
            ino: moved.id.1,
            rule: f.rule.clone(),
            class: f.class,
            method: item.action.name(),
            bytes: moved.bytes,
            private_bytes: None,
            entry: Some(f.path.clone()),
            trash: moved.place.clone().map(TrashedTo::from),
            consent: None,
            project: None,
            outcome: if moved.recycled { Outcome::Trashed } else { Outcome::RemovedNotRecycled },
        }
    }
}

fn now() -> String {
    humantime::format_rfc3339_seconds(std::time::SystemTime::now()).to_string()
}

/// A run of an area action: no path, `(dev, ino)`, rule or class.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AreaActionEntry {
    pub schema_version: u32,
    pub kind: JournalKind,
    pub ts: String,
    pub action: ActionId,
    pub area: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    /// The exact command, environment first, as a person would type it.
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_before: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_after: Option<u64>,
    pub result: ActionResult,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub output: String,
    /// What still needed a formula a `brew-uninstall` was refused for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needed_by: Vec<crate::brew::NeededBy>,
}

impl AreaActionEntry {
    pub fn new(action: &crate::free::ActionReport) -> Self {
        Self {
            schema_version: JOURNAL_SCHEMA_VERSION,
            kind: JournalKind::AreaAction,
            ts: now(),
            action: action.action,
            area: action.area.clone(),
            identifier: action.identifier.clone(),
            command: action.terminal_command.clone(),
            bytes_before: action.bytes_before,
            bytes_after: action.bytes_after,
            result: action.result.clone(),
            output: action.output.clone(),
            needed_by: action.needed_by.clone(),
        }
    }
}

/// A decision of `ddc keep remove` under the lock: the marker's folder, its
/// `(dev, ino)`, the rule it kept when the saved report knows it, and the
/// result. No class or bytes: no finding is cleaned by it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KeepRemoveEntry {
    pub schema_version: u32,
    pub kind: JournalKind,
    pub ts: String,
    #[serde(serialize_with = "crate::json_path::path")]
    pub path: PathBuf,
    pub dev: Option<u64>,
    pub ino: Option<u64>,
    pub rule: Option<String>,
    #[serde(flatten)]
    pub result: KeepResult,
}

impl KeepRemoveEntry {
    pub fn new(report: &KeepReport) -> Self {
        Self {
            schema_version: JOURNAL_SCHEMA_VERSION,
            kind: JournalKind::KeepRemove,
            ts: now(),
            path: report.path.clone(),
            dev: report.dev,
            ino: report.ino,
            rule: report.rule.clone(),
            result: report.result.clone(),
        }
    }
}

pub trait Journal {
    fn record(&mut self, entry: &JournalEntry) -> io::Result<()>;
}

/// Where `ddc disk free` journals its runs.
pub trait AreaJournal {
    fn record_area(&mut self, entry: &AreaActionEntry) -> io::Result<()>;
}

/// Where `ddc keep remove` journals its decisions.
pub trait KeepJournal {
    fn record_keep(&mut self, entry: &KeepRemoveEntry) -> io::Result<()>;
}

/// Appends to `<log-dir>/actions.jsonl`.
pub struct FileJournal {
    file: File,
    pub path: PathBuf,
}

impl FileJournal {
    pub fn open(env: &Env) -> io::Result<Self> {
        fs::create_dir_all(env.log_dir())?;
        let path = env.log_dir().join("actions.jsonl");
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self { file, path })
    }
}

impl FileJournal {
    fn append(&mut self, entry: &impl Serialize) -> io::Result<()> {
        let mut line = serde_json::to_vec(entry).map_err(io::Error::other)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.sync_data()
    }
}

impl Journal for FileJournal {
    fn record(&mut self, entry: &JournalEntry) -> io::Result<()> {
        self.append(entry)
    }
}

impl AreaJournal for FileJournal {
    fn record_area(&mut self, entry: &AreaActionEntry) -> io::Result<()> {
        self.append(entry)
    }
}

impl KeepJournal for FileJournal {
    fn record_keep(&mut self, entry: &KeepRemoveEntry) -> io::Result<()> {
        self.append(entry)
    }
}

/// In-memory journal for tests.
#[derive(Debug, Default)]
pub struct MemoryJournal(pub Vec<JournalEntry>);

impl Journal for MemoryJournal {
    fn record(&mut self, entry: &JournalEntry) -> io::Result<()> {
        self.0.push(entry.clone());
        Ok(())
    }
}

/// In-memory area journal for tests.
#[derive(Debug, Default)]
pub struct MemoryAreaJournal(pub Vec<AreaActionEntry>);

impl AreaJournal for MemoryAreaJournal {
    fn record_area(&mut self, entry: &AreaActionEntry) -> io::Result<()> {
        self.0.push(entry.clone());
        Ok(())
    }
}

/// In-memory keep journal for tests.
#[derive(Debug, Default)]
pub struct MemoryKeepJournal(pub Vec<KeepRemoveEntry>);

impl KeepJournal for MemoryKeepJournal {
    fn record_keep(&mut self, entry: &KeepRemoveEntry) -> io::Result<()> {
        self.0.push(entry.clone());
        Ok(())
    }
}
