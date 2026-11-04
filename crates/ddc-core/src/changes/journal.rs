//! The action journal as `ddc changes` reads it: leniently, any version, a
//! line that does not parse skipped. `exec::journal` writes it.

use std::fs;
use std::io::{self, ErrorKind};
use std::path::PathBuf;
use std::time::SystemTime;

use serde::Deserialize;

use crate::env::Env;

/// A line of `actions.jsonl`, with the fields a clean's credit needs.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct JournalLine {
    /// `item`, `area-action` or `keep-remove`; absent on a version 1 line,
    /// which is an item line.
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub ts: Option<String>,
    #[serde(default)]
    pub path: Option<PathBuf>,
    #[serde(default)]
    pub dev: Option<u64>,
    #[serde(default)]
    pub ino: Option<u64>,
    #[serde(default)]
    pub bytes: Option<u64>,
    /// On a session's companion: its entry's transcript.
    #[serde(default)]
    pub entry: Option<PathBuf>,
    #[serde(default)]
    pub outcome: Option<String>,
    /// On an area-action line: `brew-cleanup`, ….
    #[serde(default)]
    pub action: Option<String>,
    #[serde(default)]
    pub bytes_before: Option<u64>,
    #[serde(default)]
    pub bytes_after: Option<u64>,
    #[serde(default)]
    pub result: Option<ResultStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ResultStatus {
    #[serde(default)]
    pub status: Option<String>,
}

impl JournalLine {
    /// An item line whose item left its place: deleted, trashed, removed by
    /// its tool, or removed where the Recycle Bin refused it.
    pub(super) fn removed(&self) -> bool {
        matches!(self.kind.as_deref(), None | Some("item"))
            && matches!(self.outcome.as_deref(), Some("deleted" | "trashed" | "native" | "removed-not-recycled"))
    }

    pub(super) fn trashed(&self) -> bool {
        self.outcome.as_deref() == Some("trashed")
    }

    /// An area-action line of a `brew cleanup` whose tool ran, a failure
    /// included, as `ActionReport::ran` says.
    pub(super) fn brew_cleanup_ran(&self) -> bool {
        self.kind.as_deref() == Some("area-action")
            && self.action.as_deref() == Some("brew-cleanup")
            && matches!(
                self.result.as_ref().and_then(|r| r.status.as_deref()),
                Some("freed" | "removed-not-freed" | "still-deleting" | "failed")
            )
    }

    /// When the line was written; `None` when its `ts` does not parse.
    pub(super) fn at(&self) -> Option<SystemTime> {
        humantime::parse_rfc3339_weak(self.ts.as_deref()?).ok()
    }
}

/// The lines of a journal's text that parse, in order.
pub fn parse_journal(text: &str) -> Vec<JournalLine> {
    text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// `<log-dir>/actions.jsonl`, read whole: empty when there is none yet.
pub fn read_journal(env: &Env) -> io::Result<Vec<JournalLine>> {
    match fs::read(env.log_dir().join("actions.jsonl")) {
        Ok(bytes) => Ok(parse_journal(&String::from_utf8_lossy(&bytes))),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}
