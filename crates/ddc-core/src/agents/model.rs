//! `ddc disk agents --json` (schema `docs/agents.schema.json`).
//!
//! Values copied from a harness record (`status`, `entrypoint`, `name`,
//! `cwd`) stay strings as they were read: they are another program's
//! internals. The enums are ddc's own, and a new variant bumps the version:
//! `liveness::LiveBy` and `liveness::UnknownCause` too, which a footprint
//! carries.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::liveness::{LiveBy, UnknownCause};

/// Version of [`AgentsReport`]; the scan and disk reports have their own.
pub const AGENTS_SCHEMA_VERSION: u32 = 1;

/// What coding-agent harnesses keep under the home folder, and their
/// sessions (story "Coding agents in the Whole disk view"). Read-only, and
/// never stored: liveness is read again on every run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AgentsReport {
    pub schema_version: u32,
    pub ddc_version: String,
    /// RFC 3339, UTC.
    pub generated_at: String,
    pub duration_ms: u64,
    /// Whether the folders were walked; `false` under `--no-sizes`, and then
    /// every `bytes` is absent.
    pub sizes: bool,
    /// The harnesses with at least one folder, in the table's order.
    pub harnesses: Vec<Harness>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum HarnessId {
    ClaudeCode,
    Codex,
    Cursor,
    Opencode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Harness {
    pub id: HarnessId,
    /// "Claude Code".
    pub name: String,
    /// Its processes that run now, by pid.
    pub running: Vec<Running>,
    /// The folders that exist; a nested one lies inside the one before it.
    pub folders: Vec<Folder>,
    /// The top-level folders' bytes summed; absent without sizes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Claude Code and Codex only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Sessions>,
}

/// What ddc makes of a harness process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RunningKind {
    Cli,
    /// Codex's daemon, which holds the threads its app has open.
    AppServer,
    App,
    /// A binary of the harness that ddc cannot name, such as one inside
    /// another app's bundle.
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Running {
    pub pid: i32,
    pub kind: RunningKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Folder {
    #[serde(serialize_with = "crate::json_path::path")]
    pub path: PathBuf,
    /// Lies inside the harness's folder above it; its bytes are in that one's.
    pub nested: bool,
    /// At or under a skip of `settings.json`: never opened, no bytes.
    pub skipped: bool,
    /// Allocated bytes, each inode once; absent without sizes or when skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Folders and entries inside that could not be read: `bytes` is then a
    /// lower bound.
    pub unreadable: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RecordsState {
    Readable,
    /// No session is called stale then: a changed format must not read as
    /// "everything ended".
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum UnknownReason {
    /// The records folder does not exist.
    Missing,
    /// The records folder could not be listed.
    Unreadable,
    /// None of the candidate records parsed.
    Unparsed,
    /// At or under a skip of `settings.json`.
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Sessions {
    pub records: RecordsState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<UnknownReason>,
    /// Live first, then by start, then by id.
    pub list: Vec<Session>,
    pub transcripts: Transcripts,
    /// Claude Code only, with sizes: what each session keeps, largest
    /// total first. A session that ended has no record, so this is a list
    /// of its own, by session id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footprints: Option<Vec<SessionFootprint>>,
    /// Claude Code only, with sizes: its temporary folder outside the home
    /// folder (`/private/tmp/claude-<uid>` on macOS), when it exists. Its
    /// bytes are in no folder of the harness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temp_root: Option<TempRoot>,
}

/// Whether a session's files are still in use, as the cleanup's session
/// check would decide now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum FootprintState {
    Live,
    NotLive,
    Unknown,
}

/// What one Claude Code session keeps on disk, by kind; allocated bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionFootprint {
    /// The session id, a UUID.
    pub id: String,
    /// The project folder's name in `~/.claude/projects` (or in the
    /// temporary folder), as Claude Code wrote it; absent when only the
    /// checkpoints name the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    pub state: FootprintState,
    /// What shows it live; with `live` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_by: Option<LiveBy>,
    /// Why its state is not known; with `unknown` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_cause: Option<UnknownCause>,
    /// RFC 3339, UTC: the latest change of its transcript or a companion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    /// `~/.claude/projects/<project>/<id>.jsonl` and its companions.
    pub transcript: u64,
    /// `~/.claude/file-history/<id>`.
    pub checkpoints: u64,
    /// `<temp_root>/<project>/<id>`: its scratchpad, tasks and images.
    pub temp: u64,
    pub total: u64,
    /// Its temporary folder, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(serialize_with = "crate::json_path::opt_path")]
    pub temp_path: Option<PathBuf>,
}

/// Claude Code's temporary folder for the current user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TempRoot {
    /// Canonical.
    #[serde(serialize_with = "crate::json_path::path")]
    pub path: PathBuf,
    /// At or under a skip of `settings.json`: never opened, no bytes.
    pub skipped: bool,
    /// Allocated bytes, sessions and the rest (`bash-edit-diff`, …);
    /// absent when skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Folders and entries inside that could not be read: `bytes` is then a
    /// lower bound.
    pub unreadable: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SessionState {
    Live,
    /// A record left behind with no live process.
    Stale,
    /// The record lacks what the test needs.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Session {
    /// Claude Code's `sessionId`, or the Codex thread id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub state: SessionState,
    /// RFC 3339, UTC, from Claude Code's `startedAt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// The time in a Codex rollout's file name, as written there
    /// (`2026-10-28T09-41-07`); its time zone is not known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollout_time: Option<String>,
    /// As Claude Code wrote it (`idle`, `busy`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// As Claude Code wrote it (`cli`, `claude-desktop`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(serialize_with = "crate::json_path::opt_path")]
    pub cwd: Option<PathBuf>,
}

/// The transcripts that no live session writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Transcripts {
    /// From their names; absent when their folder is skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
    /// Allocated bytes; absent without sizes or when skipped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    pub skipped: bool,
}
