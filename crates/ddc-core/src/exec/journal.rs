//! Action journal: one JSON line per item in `<log-dir>/actions.jsonl`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::PathBuf;

use serde::Serialize;

use super::Outcome;
use crate::class::RiskClass;
use crate::env::Env;
use crate::model::SCHEMA_VERSION;
use crate::plan::PlanItem;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct JournalEntry {
    pub schema_version: u32,
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
            schema_version: SCHEMA_VERSION,
            ts: humantime::format_rfc3339_seconds(std::time::SystemTime::now()).to_string(),
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

pub trait Journal {
    fn record(&mut self, entry: &JournalEntry) -> io::Result<()>;
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

impl Journal for FileJournal {
    fn record(&mut self, entry: &JournalEntry) -> io::Result<()> {
        let mut line = serde_json::to_vec(entry).map_err(io::Error::other)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        self.file.sync_data()
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
