//! What each Claude Code session keeps on disk (story "Per-session
//! footprint: what each agent session keeps in its transcripts, temporary
//! folders and worktrees"): its transcript and companions, its checkpoints
//! and its temporary folder, joined by session id, with its liveness as the
//! cleanup's session check decides it. Read-only: names, sizes, and a
//! metadata-only look at each transcript and companion; never a
//! transcript's content.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::platform::{meta, procs};
use crate::rules::entries::{EntryShape, SessionPart, session_part};

use super::is_uuid;
use super::liveness::{self, FileFact, SessionFacts, SessionLiveness, UnknownCause};
use super::model::{FootprintState, SessionFootprint};
use super::sizes::{CHECKPOINTS, FootprintBytes, TEMP, TRANSCRIPT};

/// One session found by name, before the walk.
#[derive(Debug, Default)]
pub(super) struct Found {
    /// The project folder that names it.
    project: Option<String>,
    /// Its transcript and companions in that folder.
    files: Vec<PathBuf>,
    /// Its folder in the temporary folder.
    temp: Option<PathBuf>,
}

/// The sessions under `base` (`~/.claude`) and under `temp` (Claude Code's
/// temporary folder, canonical), by id, and the counters the walk feeds. A
/// folder `skipped` covers is never listed: the sessions it holds add
/// nothing.
pub(super) fn collect(
    base: &Path,
    temp: Option<&Path>,
    skipped: &dyn Fn(&Path) -> bool,
) -> (BTreeMap<String, Found>, FootprintBytes) {
    let mut found: BTreeMap<String, Found> = BTreeMap::new();
    let projects = base.join("projects");
    for (slug, dir) in children(&projects, skipped) {
        for (name, path, is_dir) in entries(&dir, skipped) {
            let id = match session_part(EntryShape::Transcript, name.as_bytes(), is_dir) {
                SessionPart::Key(id) | SessionPart::Companion(id) => id,
                SessionPart::None => continue,
            };
            let f = found.entry(id.to_owned()).or_default();
            f.project.get_or_insert_with(|| slug.clone());
            f.files.push(path);
        }
    }
    let history = base.join("file-history");
    for (id, _) in children(&history, skipped).into_iter().filter(|(id, _)| is_uuid(id)) {
        found.entry(id).or_default();
    }
    for (slug, dir) in temp.map(|t| children(t, skipped)).unwrap_or_default() {
        for (id, path) in children(&dir, skipped).into_iter().filter(|(id, _)| is_uuid(id)) {
            let f = found.entry(id).or_default();
            f.project.get_or_insert_with(|| slug.clone());
            f.temp = Some(path);
        }
    }
    let ids: Vec<&str> = found.keys().map(String::as_str).collect();
    let bytes = FootprintBytes::new(projects, history, temp.map(Path::to_path_buf), &ids);
    (found, bytes)
}

/// Each session's footprint, largest total first. `records` is
/// `~/.claude/sessions`, `records_skipped` whether a skip covers it; `pids`
/// are the user's processes.
pub(super) fn assemble(
    found: BTreeMap<String, Found>,
    bytes: &FootprintBytes,
    records: &Path,
    records_skipped: bool,
    pids: &[i32],
    now: SystemTime,
) -> Vec<SessionFootprint> {
    let facts: Vec<SessionFacts> = found
        .iter()
        .map(|(id, f)| SessionFacts { session: id.clone(), files: f.files.iter().filter_map(|p| fact(p)).collect() })
        .collect();
    let read = if records_skipped {
        Err(UnknownCause::RecordsSkipped)
    } else {
        liveness::claude_records(records, procs::process_start)
    };
    let pids = if read.is_ok() { pids } else { &[] };
    let states = liveness::claude(read.as_deref().map_err(|c| *c), &facts, pids, procs::open_vnodes, now);

    let mut out: Vec<SessionFootprint> = found
        .into_iter()
        .zip(facts.iter().zip(states))
        .map(|((id, f), (facts, state))| {
            let kinds = bytes.of(&id);
            let (state, live_by, unknown_cause) = match state {
                SessionLiveness::Live { by } => (FootprintState::Live, Some(by), None),
                SessionLiveness::Unknown { cause } => (FootprintState::Unknown, None, Some(cause)),
                SessionLiveness::NotLive => (FootprintState::NotLive, None, None),
            };
            let modified = facts.files.iter().filter_map(|f| f.modified).max().and_then(rfc3339);
            SessionFootprint {
                id,
                project: f.project,
                state,
                live_by,
                unknown_cause,
                modified,
                transcript: kinds[TRANSCRIPT],
                checkpoints: kinds[CHECKPOINTS],
                temp: kinds[TEMP],
                total: kinds.iter().sum(),
                temp_path: f.temp,
            }
        })
        .collect();
    out.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.id.cmp(&b.id)));
    out
}

/// A file's identity and its own modification time, from its metadata
/// alone; `None` when it is gone.
fn fact(path: &Path) -> Option<FileFact> {
    let md = path.symlink_metadata().ok()?;
    Some(FileFact { id: meta::file_id(&md).ok()?, modified: Some(meta::mtime(&md)) })
}

fn rfc3339(secs: i64) -> Option<String> {
    let at = UNIX_EPOCH + Duration::from_secs(u64::try_from(secs).ok()?);
    Some(humantime::format_rfc3339_seconds(at).to_string())
}

/// The folders in `dir` with names that are UTF-8, by name; none when a
/// skip covers `dir`, and none of those a skip covers.
fn children(dir: &Path, skipped: &dyn Fn(&Path) -> bool) -> Vec<(String, PathBuf)> {
    entries(dir, skipped).into_iter().filter(|(_, _, is_dir)| *is_dir).map(|(n, p, _)| (n, p)).collect()
}

/// Every entry in `dir` with a name that is UTF-8, and whether it is a
/// folder (a symlink is not); none when a skip covers `dir`, and no folder
/// a skip covers.
fn entries(dir: &Path, skipped: &dyn Fn(&Path) -> bool) -> Vec<(String, PathBuf, bool)> {
    if skipped(dir) {
        return Vec::new();
    }
    let mut out = Vec::new();
    for entry in crate::stats::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(name) = entry.file_name().into_string() else { continue };
        let path = entry.path();
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if is_dir && skipped(&path) {
            continue;
        }
        out.push((name, path, is_dir));
    }
    out.sort();
    out
}
