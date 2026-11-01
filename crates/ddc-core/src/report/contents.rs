//! What a folder with entries holds: the user's own folders (story "User
//! data: pick your own files for the Trash") and the coding agents' session
//! folders (story "Harness data rules", epic DDC-003). The report's
//! `contents` of each folder, and the full list of every entry and its
//! state, which `plan_listed` checks a picked entry against, not the
//! report's top 100.
//!
//! A session folder's entry is keyed by its transcript (`<session>.jsonl`)
//! or, in `file-history`, by its folder (`<session>/`); its companions,
//! the siblings named after the same session, count in its bytes and its
//! lock. Its liveness is read once per scan, only when a session was found
//! (ADR "Agent footprint: worktrees, harness data and bounded consent for
//! agents", §3).

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::backups;
use crate::agents::liveness::{self, FileFact, SessionFacts, SessionLiveness, UnknownCause};
use crate::exec::ProcessTable;
use crate::model::{Backup, Contents, Finding, FolderEntry, FolderEntryKind};
use crate::platform::PLATFORM;
use crate::rules::AtOrAbove;
use crate::rules::entries::{
    BACKUP_RULE, EntryFacts, EntryLiveness, EntryLock, EntryPolicy, EntryShape, SessionPart, lock_of, policy,
    session_part,
};
use crate::rules::{KEPT, protected_in_home};
use crate::scan::reader::SF_DATALESS;
use crate::scan::{EntryKind, EntryResult, OwnerEntries};
use crate::stats::{self, Counter};

/// Where Claude Code keeps its live session records, from the home folder.
pub const CLAUDE_RECORDS: &str = ".claude/sessions";

/// How much of a folder the report lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentsLimits {
    pub max: usize,
    /// The floor of a folder whose policy has one (the user's own folders).
    pub min_bytes: u64,
}

impl Default for ContentsLimits {
    fn default() -> Self {
        Self { max: 100, min_bytes: 10 << 20 }
    }
}

/// Every entry of every folder with entries the scan read, by folder path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryIndex {
    pub folders: HashMap<PathBuf, IndexedFolder>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedFolder {
    pub rule: String,
    pub dev: u64,
    pub ino: u64,
    /// By name, byte for byte. A session's entry is under its key's name.
    pub entries: HashMap<OsString, IndexedEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedEntry {
    pub dev: u64,
    pub ino: u64,
    pub is_dir: bool,
    /// A session's entry: its companions' bytes included.
    pub bytes: u64,
    pub incomplete: bool,
    pub state: EntryState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryState {
    Pickable,
    Locked(EntryLock),
    NotAnEntry(NotAnEntry),
}

/// A child of a folder that is never an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotAnEntry {
    Symlink,
    /// A FIFO, a socket, a device.
    Other,
    /// Its content lives in the cloud.
    Dataless,
    /// Its own metadata could not be read.
    Unreadable,
    /// A finding of its own (another rule's, or `user.kept`).
    Finding,
    /// In a session folder: a companion of a session's entry, which moves
    /// with it and is never picked alone.
    Companion,
    /// In a session folder: no session's entry or companion (`memory/`,
    /// `sessions-index.json`, a companion without its session's key).
    NotASession,
}

impl EntryIndex {
    pub fn entry(&self, folder: &Path, name: &std::ffi::OsStr) -> Option<(&IndexedFolder, Option<&IndexedEntry>)> {
        let f = self.folders.get(folder)?;
        Some((f, f.entries.get(name)))
    }
}

/// What the scan knows besides the walk: the excludes, the directories that
/// hold a keep marker inside findings, the scan's start, and, for the
/// session folders, the skips and the processes.
pub(crate) struct Context<'a> {
    pub home: &'a Path,
    pub now: SystemTime,
    pub excludes: &'a [PathBuf],
    pub markers: &'a [PathBuf],
    pub limits: ContentsLimits,
    /// Folders never opened: Claude Code's records under one are not read.
    pub skips: &'a [PathBuf],
    /// Asked only when a session was found.
    pub processes: &'a dyn ProcessTable,
}

/// One entry of a folder before its lock is known: a child, or a session's
/// key with its companions.
struct Unit<'w> {
    path: PathBuf,
    key: &'w EntryResult,
    companions: Vec<&'w EntryResult>,
    state: Pending,
    bytes: u64,
    incomplete: bool,
    /// The newest own modification time of its files.
    modified: Option<i64>,
}

/// A unit's state: decided, or a session entry's lock reasons waiting for
/// its session's liveness.
enum Pending {
    Done(EntryState),
    Session { session: String, kept: bool, protected: bool, other_volume: bool },
}

/// One folder's children, read into units, before the liveness is read.
struct Folder<'w> {
    finding: usize,
    policy: &'static EntryPolicy,
    units: Vec<Unit<'w>>,
    /// The children that are no entry, with their state.
    others: Vec<(&'w EntryResult, NotAnEntry)>,
}

/// Fills `contents` of each folder finding and returns the full index.
pub(crate) fn attach(findings: &mut [Finding], walked: &[OwnerEntries], cx: &Context<'_>) -> EntryIndex {
    // Compared by the platform's key: on Windows a catalog spelling and the
    // disk's may differ in case and separator.
    let protected: Vec<PathBuf> = protected_in_home(PLATFORM)
        .iter()
        .map(|rel| crate::platform::paths::key(&cx.home.join(rel)).into_owned())
        .collect();
    let others: Vec<(PathBuf, bool)> = findings.iter().map(|f| (f.path.clone(), f.rule == KEPT)).collect();
    let known = Known {
        found: others.iter().map(|(p, _)| p.as_path()).collect(),
        kept: AtOrAbove::new(
            others
                .iter()
                .filter(|(_, kept)| *kept)
                .map(|(p, _)| p.as_path())
                .chain(cx.markers.iter().map(PathBuf::as_path)),
        ),
    };
    let mut folders: Vec<Folder<'_>> = Vec::new();
    for (i, folder) in findings.iter().enumerate() {
        let Some(policy) = policy(&folder.rule) else { continue };
        let Some(children) = walked.iter().find(|w| w.owner == folder.path) else { continue };
        let read = Reading { folder: &folder.path, rule: &folder.rule, known: &known, protected: &protected, cx };
        folders.push(match policy.shape {
            EntryShape::Child => read.children(i, policy, &children.entries),
            EntryShape::Transcript | EntryShape::SessionDir => read.sessions(i, policy, &children.entries),
        });
    }
    let live = sessions_liveness(&folders, cx);

    let mut index = EntryIndex::default();
    for folder in folders {
        let finding = &findings[folder.finding];
        let (path, rule) = (finding.path.clone(), finding.rule.clone());
        // Only the findings inside this folder can be at or in one of its entries.
        let inside: Vec<(PathBuf, bool)> = others
            .iter()
            .filter(|(p, _)| stats::counted(Counter::ClaimLookups, || p != &path && p.starts_with(&path)))
            .cloned()
            .collect();
        let mut indexed = HashMap::with_capacity(folder.units.len() + folder.others.len());
        let mut listable = Vec::new();
        let (mut rest_count, mut rest_bytes) = (0u64, 0u64);
        for (child, why) in &folder.others {
            if why != &NotAnEntry::Finding && why != &NotAnEntry::Companion {
                rest_count += 1;
                rest_bytes += child.bytes;
            }
            let entry = IndexedEntry {
                dev: child.dev,
                ino: child.ino,
                is_dir: child.kind == EntryKind::Dir,
                bytes: child.bytes,
                incomplete: child.errors > 0 || child.skipped > 0,
                state: EntryState::NotAnEntry(*why),
            };
            indexed.insert(child.name.clone(), entry);
        }
        for unit in folder.units {
            let state = match unit.state {
                Pending::Done(state) => state,
                Pending::Session { ref session, kept, protected, other_volume } => {
                    let facts = EntryFacts {
                        home: cx.home,
                        rule: &rule,
                        path: &unit.path,
                        modified: unit.modified,
                        added: unit.key.added,
                        now: cx.now,
                        kept,
                        protected,
                        other_volume,
                        finished: false,
                        session: Some(
                            live.get(session)
                                .copied()
                                .unwrap_or(SessionLiveness::Unknown { cause: UnknownCause::SessionWithoutFiles }),
                        ),
                    };
                    lock_of(&facts).map_or(EntryState::Pickable, EntryState::Locked)
                }
            };
            let entry = IndexedEntry {
                dev: unit.key.dev,
                ino: unit.key.ino,
                is_dir: unit.key.kind == EntryKind::Dir,
                bytes: unit.bytes,
                incomplete: unit.incomplete,
                state,
            };
            match &entry.state {
                EntryState::NotAnEntry(NotAnEntry::Finding) => {}
                EntryState::NotAnEntry(_) => {
                    rest_count += 1;
                    rest_bytes += entry.bytes;
                }
                _ if folder.policy.floor && entry.bytes < cx.limits.min_bytes => {
                    rest_count += 1;
                    rest_bytes += entry.bytes;
                }
                _ => listable.push((unit.path, unit.modified, entry.clone())),
            }
            indexed.insert(unit.key.name.clone(), entry);
        }
        // Largest first; ties by path, which within one folder is by name.
        listable.sort_by(|a, b| b.2.bytes.cmp(&a.2.bytes).then_with(|| a.0.as_os_str().cmp(b.0.as_os_str())));
        for (_, _, e) in listable.iter().skip(cx.limits.max) {
            rest_count += 1;
            rest_bytes += e.bytes;
        }
        listable.truncate(cx.limits.max);
        let entries = listable
            .into_iter()
            .map(|(path, modified, e)| listed(path, modified, e, &rule, folder.policy, &inside))
            .collect();
        let finding = &mut findings[folder.finding];
        finding.contents = Some(Contents { entries, rest_count, rest_bytes });
        index.folders.insert(path, IndexedFolder { rule, dev: finding.dev, ino: finding.ino, entries: indexed });
    }
    index
}

/// The findings, and the folders that hold a kept finding or a keep marker.
struct Known<'a> {
    found: HashSet<&'a Path>,
    kept: AtOrAbove,
}

/// What reading one folder's children needs.
struct Reading<'a> {
    folder: &'a Path,
    rule: &'a str,
    known: &'a Known<'a>,
    protected: &'a [PathBuf],
    cx: &'a Context<'a>,
}

impl Reading<'_> {
    /// The user's own folder: each child is an entry of its own.
    fn children<'w>(&self, finding: usize, policy: &'static EntryPolicy, children: &'w [EntryResult]) -> Folder<'w> {
        let units = children
            .iter()
            .map(|child| {
                let path = self.folder.join(&child.name);
                let state = state_of(child, &path, self.rule, self.known, self.protected, self.cx);
                Unit {
                    path,
                    key: child,
                    companions: Vec::new(),
                    state: Pending::Done(state),
                    bytes: child.bytes,
                    incomplete: child.errors > 0 || child.skipped > 0,
                    modified: child.modified,
                }
            })
            .collect();
        Folder { finding, policy, units, others: Vec::new() }
    }

    /// A session folder: one entry per session key, its companions with it;
    /// every other child is no entry.
    fn sessions<'w>(&self, finding: usize, policy: &'static EntryPolicy, children: &'w [EntryResult]) -> Folder<'w> {
        let mut others = Vec::new();
        let mut keys: HashMap<&'w str, Unit<'w>> = HashMap::new();
        let mut companions: Vec<(&'w str, &'w EntryResult)> = Vec::new();
        let mut broken: Vec<&'w str> = Vec::new();
        for child in children {
            let path = self.folder.join(&child.name);
            let name = child.name.as_encoded_bytes();
            if let Some(why) = not_an_entry(child, &path, self.known) {
                // A companion that cannot move as it is leaves its session's entry incomplete.
                if let SessionPart::Key(s) | SessionPart::Companion(s) = session_part(policy.shape, name, false) {
                    broken.push(s);
                }
                others.push((child, why));
                continue;
            }
            match session_part(policy.shape, name, child.kind == EntryKind::Dir) {
                SessionPart::Key(session) => {
                    keys.insert(
                        session,
                        Unit {
                            path,
                            key: child,
                            companions: Vec::new(),
                            state: Pending::Session {
                                session: session.to_owned(),
                                kept: false,
                                protected: false,
                                other_volume: false,
                            },
                            bytes: 0,
                            incomplete: false,
                            modified: None,
                        },
                    );
                }
                SessionPart::Companion(session) => companions.push((session, child)),
                SessionPart::None => others.push((child, NotAnEntry::NotASession)),
            }
        }
        for (session, child) in companions {
            match keys.get_mut(session) {
                Some(unit) => {
                    unit.companions.push(child);
                    others.push((child, NotAnEntry::Companion));
                }
                None => others.push((child, NotAnEntry::NotASession)),
            }
        }
        let mut units: Vec<Unit<'w>> = keys.into_values().collect();
        for unit in &mut units {
            let files: Vec<(&EntryResult, PathBuf)> = std::iter::once(unit.key)
                .chain(unit.companions.iter().copied())
                .map(|c| (c, self.folder.join(&c.name)))
                .collect();
            unit.bytes = files.iter().map(|(c, _)| c.bytes).sum();
            unit.modified = files.iter().filter_map(|(c, _)| c.modified).max();
            let session = match &unit.state {
                Pending::Session { session, .. } => session.clone(),
                Pending::Done(_) => continue,
            };
            unit.incomplete =
                files.iter().any(|(c, _)| c.errors > 0 || c.skipped > 0) || broken.contains(&session.as_str());
            unit.state = Pending::Session {
                kept: files.iter().any(|(_, p)| self.kept(p)),
                protected: files
                    .iter()
                    .any(|(_, p)| self.protected.iter().any(|x| x.starts_with(crate::platform::paths::key(p)))),
                other_volume: files.iter().any(|(c, _)| c.other_fs > 0),
                session,
            };
        }
        units.sort_by(|a, b| a.path.cmp(&b.path));
        Folder { finding, policy, units, others }
    }

    /// A keep marker in it or above it, or an exclude at or in it.
    fn kept(&self, path: &Path) -> bool {
        self.known.kept.contains(path)
            || self.cx.excludes.iter().any(|e| stats::counted(Counter::AncestorChecks, || e.starts_with(path)))
    }
}

/// Every session of the session folders whose liveness is read by Claude
/// Code's session check, decided at once: one read of the records and,
/// for the sessions still undecided, one sweep of the open files. Nothing
/// is read when no session was found.
fn sessions_liveness(folders: &[Folder<'_>], cx: &Context<'_>) -> HashMap<String, SessionLiveness> {
    let sessions: Vec<(&str, SessionFacts)> = folders
        .iter()
        .filter(|f| f.policy.liveness == EntryLiveness::ClaudeSession)
        .flat_map(|f| &f.units)
        .filter_map(|u| match &u.state {
            Pending::Session { session, .. } => Some((session.as_str(), u)),
            Pending::Done(_) => None,
        })
        .map(|(session, u)| {
            let files = std::iter::once(u.key)
                .chain(u.companions.iter().copied())
                .map(|c| FileFact { id: (c.dev, c.ino), modified: c.modified })
                .collect();
            (session, SessionFacts { session: session.to_owned(), files })
        })
        .collect();
    if sessions.is_empty() {
        return HashMap::new();
    }
    // One fact per session: a session in both folders has the files of both.
    let mut merged: Vec<SessionFacts> = Vec::new();
    for (session, facts) in &sessions {
        match merged.iter_mut().find(|m| m.session == *session) {
            Some(m) => m.files.extend(facts.files.iter().copied()),
            None => merged.push(facts.clone()),
        }
    }
    let dir = cx.home.join(CLAUDE_RECORDS);
    let records = if cx.skips.iter().any(|s| dir.starts_with(s)) {
        Err(UnknownCause::RecordsSkipped)
    } else {
        liveness::claude_records(&dir, |pid| cx.processes.process_start(pid))
    };
    let pids: Vec<i32> = match &records {
        Ok(_) => cx.processes.pids(),
        Err(_) => Vec::new(),
    };
    let states = liveness::claude(
        records.as_deref().map_err(|c| *c),
        &merged,
        &pids,
        |pid| cx.processes.open_vnodes(pid),
        cx.now,
    );
    merged.into_iter().map(|m| m.session).zip(states).collect()
}

/// Why a child is never an entry, from what the walk saw of it alone.
fn not_an_entry(child: &EntryResult, path: &Path, known: &Known<'_>) -> Option<NotAnEntry> {
    if child.error.is_some() {
        return Some(NotAnEntry::Unreadable);
    }
    match child.kind {
        EntryKind::Symlink => return Some(NotAnEntry::Symlink),
        EntryKind::Other => return Some(NotAnEntry::Other),
        EntryKind::Dir | EntryKind::File => {}
    }
    if child.flags & SF_DATALESS != 0 {
        return Some(NotAnEntry::Dataless);
    }
    if stats::counted(Counter::ClaimLookups, || known.found.contains(path)) {
        return Some(NotAnEntry::Finding);
    }
    None
}

fn state_of(
    child: &EntryResult,
    path: &Path,
    rule: &str,
    known: &Known<'_>,
    protected: &[PathBuf],
    cx: &Context<'_>,
) -> EntryState {
    if let Some(why) = not_an_entry(child, path, known) {
        return EntryState::NotAnEntry(why);
    }
    let kept = known.kept.contains(path)
        || cx.excludes.iter().any(|e| stats::counted(Counter::AncestorChecks, || e.starts_with(path)));
    let facts = EntryFacts {
        home: cx.home,
        rule,
        path,
        modified: child.modified,
        added: child.added,
        now: cx.now,
        kept,
        protected: protected.iter().any(|p| p.starts_with(crate::platform::paths::key(path))),
        other_volume: child.other_fs > 0,
        finished: rule == BACKUP_RULE && child.kind == EntryKind::Dir && backups::finished(path),
        session: None,
    };
    match lock_of(&facts) {
        Some(lock) => EntryState::Locked(lock),
        None => EntryState::Pickable,
    }
}

/// The report's line for a listed entry: what only the listed ones need.
fn listed(
    path: PathBuf,
    modified: Option<i64>,
    e: IndexedEntry,
    rule: &str,
    policy: &EntryPolicy,
    others: &[(PathBuf, bool)],
) -> FolderEntry {
    let is_dir = e.is_dir;
    let backup = (rule == BACKUP_RULE && is_dir)
        .then(|| backups::info(&path))
        .map(|info| Backup { device_name: info.device_name, last_backup: info.last_backup.and_then(super::rfc3339) });
    FolderEntry {
        kind: if is_dir { FolderEntryKind::Folder } else { FolderEntryKind::File },
        bytes: e.bytes,
        dev: e.dev,
        ino: e.ino,
        modified: modified.and_then(super::rfc3339_secs),
        lock: match e.state {
            EntryState::Locked(lock) => Some(lock),
            _ => None,
        },
        incomplete: e.incomplete,
        findings: others
            .iter()
            .filter(|(p, _)| stats::counted(Counter::ClaimLookups, || p != &path && p.starts_with(&path)))
            .count() as u64,
        git: policy.user_data && is_dir && fs::symlink_metadata(path.join(".git")).is_ok(),
        backup: backup.filter(|b| b.device_name.is_some() || b.last_backup.is_some()),
        path,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn child(name: &str, kind: EntryKind) -> EntryResult {
        EntryResult {
            name: name.into(),
            kind,
            dev: 1,
            ino: 2,
            flags: 0,
            error: None,
            modified: Some(1_000_000_000),
            added: Some(1_000_000_000),
            bytes: 4_096,
            errors: 0,
            skipped: 0,
            other_fs: 0,
        }
    }

    fn state(child: &EntryResult) -> EntryState {
        let home = Path::new("/Users/example");
        let cx = Context {
            home,
            now: UNIX_EPOCH + Duration::from_secs(2_000_000_000),
            excludes: &[],
            markers: &[],
            limits: ContentsLimits::default(),
            skips: &[],
            processes: &crate::exec::SystemProcesses,
        };
        let path = home.join("Downloads").join(&child.name);
        let known = Known { found: HashSet::new(), kept: AtOrAbove::new([]) };
        state_of(child, &path, "user.downloads", &known, &[], &cx)
    }

    #[test]
    fn what_the_walk_could_not_see_decides_the_state() {
        assert_eq!(state(&child("a", EntryKind::File)), EntryState::Pickable);
        let mut volume = child("v", EntryKind::Dir);
        volume.other_fs = 1;
        assert_eq!(state(&volume), EntryState::Locked(EntryLock::OtherVolume));
        let mut cloud = child("c", EntryKind::Dir);
        cloud.flags = SF_DATALESS;
        assert_eq!(state(&cloud), EntryState::NotAnEntry(NotAnEntry::Dataless));
        let mut file = child("f", EntryKind::File);
        file.flags = SF_DATALESS;
        assert_eq!(state(&file), EntryState::NotAnEntry(NotAnEntry::Dataless), "a dataless file too");
        let mut broken = child("b", EntryKind::File);
        broken.error = Some(crate::platform::errno::EACCES);
        assert_eq!(state(&broken), EntryState::NotAnEntry(NotAnEntry::Unreadable));
        assert_eq!(state(&child("p", EntryKind::Other)), EntryState::NotAnEntry(NotAnEntry::Other));
        assert_eq!(state(&child("l", EntryKind::Symlink)), EntryState::NotAnEntry(NotAnEntry::Symlink));
        let mut partly = child("d", EntryKind::Dir);
        partly.errors = 2;
        assert_eq!(state(&partly), EntryState::Pickable, "partly unreadable stays pickable");
    }
}
