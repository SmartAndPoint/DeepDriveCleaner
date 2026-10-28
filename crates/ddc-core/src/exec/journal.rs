//! Action journal: one JSON line per item in `<log-dir>/actions.jsonl`, and
//! one per area action (safety ADR, amendment of 2026-10-28).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::PathBuf;

use serde::Serialize;

use super::Outcome;
use crate::class::RiskClass;
use crate::disk::ActionId;
use crate::env::Env;
use crate::free::ActionResult;
use crate::plan::PlanItem;

/// The journal's own version. Its lines carried the scan report's
/// `SCHEMA_VERSION` (1) until area actions came, so it starts at 2; a
/// version 1 line has no `kind` and is an item line.
pub const JOURNAL_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum JournalKind {
    Item,
    AreaAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JournalEntry {
    pub schema_version: u32,
    pub kind: JournalKind,
    pub ts: String,
    pub path: PathBuf,
    pub dev: u64,
    pub ino: u64,
    pub rule: String,
    pub class: RiskClass,
    pub method: &'static str,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub private_bytes: Option<u64>,
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
            outcome: outcome.clone(),
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
