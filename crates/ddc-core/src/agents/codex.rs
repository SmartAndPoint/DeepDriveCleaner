//! Codex's threads: one `thread-writer-locks/<id>.lock` per thread, open in
//! a process of the user while the thread is open in Codex. Only the lock
//! files are `stat`ed; the open files of the processes are matched by the
//! kernel's `(dev, ino)` and never `stat`ed, since a file in a protected
//! folder could make macOS ask. No lock is ever taken.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::platform::procs::OpenVnode;

use super::liveness::CodexLocks;
use super::{RecordsState, Session, SessionState, UnknownReason};

/// The threads of `dir` (`~/.codex/thread-writer-locks`): their locks, and
/// whether a process of `pids` holds each open. `rollouts` maps a thread
/// id to its rollout's time.
pub(crate) fn threads(
    dir: &Path,
    pids: &[i32],
    open: impl Fn(i32) -> Vec<OpenVnode>,
    rollouts: &HashMap<String, String>,
) -> (RecordsState, Option<UnknownReason>, Vec<Session>) {
    let locks = match locks(dir) {
        Ok(locks) => locks,
        Err(reason) => return (RecordsState::Unknown, Some(reason), Vec::new()),
    };
    let held = held(&locks.iter().map(|(_, k)| *k).collect(), pids, open);
    let list = locks
        .into_iter()
        .map(|(id, key)| Session {
            state: if held.contains(&key) { SessionState::Live } else { SessionState::Stale },
            rollout_time: rollouts.get(&id).cloned(),
            id: Some(id),
            started_at: None,
            status: None,
            entrypoint: None,
            name: None,
            cwd: None,
        })
        .collect();
    (RecordsState::Readable, None, list)
}

/// The thread locks of `dir` (`~/.codex/thread-writer-locks`) by thread id,
/// sorted, each with its `(dev, ino)`; `Missing` or `Unreadable` when the
/// folder cannot be listed.
pub(crate) fn locks(dir: &Path) -> CodexLocks {
    let entries = match crate::stats::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(UnknownReason::Missing),
        Err(_) => return Err(UnknownReason::Unreadable),
    };
    // Only `<thread-id>.lock`: `.coordination.lock` names no thread.
    let mut locks: Vec<(String, (u64, u64))> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let id = name.to_str()?.strip_suffix(".lock")?;
            if id.is_empty() || id.starts_with('.') {
                return None;
            }
            let md = std::fs::symlink_metadata(e.path()).ok().filter(|m| m.is_file())?;
            Some((id.to_owned(), crate::platform::meta::file_id(&md).ok()?))
        })
        .collect();
    locks.sort();
    Ok(locks)
}

/// The locks some process of `pids` holds open, read pid by pid until all
/// are found. Kept apart so a source test can see it opens and `stat`s
/// nothing.
pub(crate) fn held(
    locks: &HashSet<(u64, u64)>,
    pids: &[i32],
    open: impl Fn(i32) -> Vec<OpenVnode>,
) -> HashSet<(u64, u64)> {
    let mut found = HashSet::new();
    for &pid in pids {
        if found.len() == locks.len() {
            break;
        }
        for v in open(pid) {
            if locks.contains(&(v.dev, v.ino)) {
                found.insert((v.dev, v.ino));
            }
        }
    }
    found
}

/// Why [`rollouts`] lists nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Absent {
    /// `sessions` itself could not be read.
    Unreadable,
    /// A skip covers `sessions` or a folder inside it, which is never opened.
    Skipped,
}

/// The rollouts under `sessions` (`YYYY/MM/DD/rollout-<time>-<id>.jsonl`),
/// by thread id, with their time as written. Names only: no file is opened
/// or `stat`ed.
pub(super) fn rollouts(sessions: &Path, skipped: &dyn Fn(&Path) -> bool) -> Result<HashMap<String, String>, Absent> {
    let mut out = HashMap::new();
    let mut dirs = vec![(sessions.to_path_buf(), 0)];
    while let Some((dir, depth)) = dirs.pop() {
        if skipped(&dir) {
            return Err(Absent::Skipped);
        }
        let entries = match crate::stats::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) if depth > 0 => continue,
            Err(_) => return Err(Absent::Unreadable),
        };
        for entry in entries.flatten() {
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            if depth < 3 && is_dir {
                dirs.push((entry.path(), depth + 1));
            } else if depth == 3
                && !is_dir
                && let Some((time, id)) = entry.file_name().to_str().and_then(rollout_name)
            {
                out.insert(id.to_owned(), time.to_owned());
            }
        }
    }
    Ok(out)
}

/// `rollout-<time>-<id>.jsonl` as `(time, id)`; the id is a UUID.
pub(super) fn rollout_name(name: &str) -> Option<(&str, &str)> {
    let stem = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    let split = stem.len().checked_sub(37)?;
    let (time, id) = (stem.get(..split)?, stem.get(split + 1..)?);
    (stem.as_bytes()[split] == b'-' && !time.is_empty() && super::is_uuid(id)).then_some((time, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rollout_name_gives_its_time_and_thread() {
        let id = "019a2b3c-4d5e-7f60-8a9b-0c1d2e3f4a5b";
        assert_eq!(rollout_name(&format!("rollout-2026-10-28T09-41-07-{id}.jsonl")), Some(("2026-10-28T09-41-07", id)));
        assert_eq!(rollout_name(&format!("rollout-{id}.jsonl")), None);
        assert_eq!(rollout_name("rollout-2026-10-28T09-41-07-notanid.jsonl"), None);
        assert_eq!(rollout_name(&format!("history-2026-{id}.jsonl")), None);
    }

    #[test]
    fn the_sweep_stops_once_every_lock_is_found() {
        let locks: HashSet<(u64, u64)> = [(1, 10)].into();
        let asked = std::cell::Cell::new(0);
        let open = |pid: i32| {
            asked.set(asked.get() + 1);
            if pid == 2 { vec![OpenVnode { dev: 1, ino: 10, path: None }] } else { Vec::new() }
        };
        assert_eq!(held(&locks, &[1, 2, 3, 4], open), locks);
        assert_eq!(asked.get(), 2);
    }
}
