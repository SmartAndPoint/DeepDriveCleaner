//! What the user's own folders hold (story "User data: pick your own files
//! for the Trash"): the report's `contents` of each folder, and the full
//! list of every entry and its state, which `plan_listed` checks a picked
//! entry against, not the report's top 100.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::backups;
use crate::model::{Backup, Contents, Finding, FolderEntry, FolderEntryKind};
use crate::rules::entries::{BACKUP_RULE, ENTRY_RULES, EntryFacts, EntryLock, lock_of};
use crate::rules::{KEPT, PROTECTED_IN_HOME};
use crate::scan::reader::SF_DATALESS;
use crate::scan::{EntryKind, EntryResult, OwnerEntries};
use crate::stats::{self, Counter};

/// How much of a folder the report lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentsLimits {
    pub max: usize,
    pub min_bytes: u64,
}

impl Default for ContentsLimits {
    fn default() -> Self {
        Self { max: 100, min_bytes: 10 << 20 }
    }
}

/// Every entry of every user-data folder the scan read, by folder path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryIndex {
    pub folders: HashMap<PathBuf, IndexedFolder>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedFolder {
    pub rule: String,
    pub dev: u64,
    pub ino: u64,
    /// By name, byte for byte.
    pub entries: HashMap<OsString, IndexedEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedEntry {
    pub dev: u64,
    pub ino: u64,
    pub is_dir: bool,
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
}

impl EntryIndex {
    pub fn entry(&self, folder: &Path, name: &std::ffi::OsStr) -> Option<(&IndexedFolder, Option<&IndexedEntry>)> {
        let f = self.folders.get(folder)?;
        Some((f, f.entries.get(name)))
    }
}

/// What the scan knows besides the walk: the excludes, the directories that
/// hold a keep marker inside findings, and the scan's start.
pub(crate) struct Context<'a> {
    pub home: &'a Path,
    pub now: SystemTime,
    pub excludes: &'a [PathBuf],
    pub markers: &'a [PathBuf],
    pub limits: ContentsLimits,
}

/// Fills `contents` of each folder finding and returns the full index.
pub(crate) fn attach(findings: &mut [Finding], walked: &[OwnerEntries], cx: &Context<'_>) -> EntryIndex {
    let protected: Vec<PathBuf> = PROTECTED_IN_HOME.iter().map(|rel| cx.home.join(rel)).collect();
    let others: Vec<(PathBuf, bool)> = findings.iter().map(|f| (f.path.clone(), f.rule == KEPT)).collect();
    let mut index = EntryIndex::default();
    for folder in findings.iter_mut().filter(|f| ENTRY_RULES.contains(&f.rule.as_str())) {
        let Some(children) = walked.iter().find(|w| w.owner == folder.path) else { continue };
        // Only the findings inside this folder can be at or in one of its entries.
        let others: Vec<(PathBuf, bool)> =
            others.iter().filter(|(p, _)| p != &folder.path && p.starts_with(&folder.path)).cloned().collect();
        let mut indexed = HashMap::with_capacity(children.entries.len());
        let mut listable = Vec::new();
        let (mut rest_count, mut rest_bytes) = (0u64, 0u64);
        for child in &children.entries {
            let path = folder.path.join(&child.name);
            let state = state_of(child, &path, &folder.rule, &others, &protected, cx);
            let entry = IndexedEntry {
                dev: child.dev,
                ino: child.ino,
                is_dir: child.kind == EntryKind::Dir,
                bytes: child.bytes,
                incomplete: child.errors > 0 || child.skipped > 0,
                state,
            };
            match &entry.state {
                EntryState::NotAnEntry(NotAnEntry::Finding) => {}
                EntryState::NotAnEntry(_) => {
                    rest_count += 1;
                    rest_bytes += entry.bytes;
                }
                _ if entry.bytes < cx.limits.min_bytes => {
                    rest_count += 1;
                    rest_bytes += entry.bytes;
                }
                _ => listable.push((path, child, entry.clone())),
            }
            indexed.insert(child.name.clone(), entry);
        }
        listable.sort_by(|a, b| b.2.bytes.cmp(&a.2.bytes).then_with(|| a.0.as_os_str().cmp(b.0.as_os_str())));
        for (_, _, e) in listable.iter().skip(cx.limits.max) {
            rest_count += 1;
            rest_bytes += e.bytes;
        }
        listable.truncate(cx.limits.max);
        let entries =
            listable.into_iter().map(|(path, child, e)| listed(path, child, e, &folder.rule, &others)).collect();
        folder.contents = Some(Contents { entries, rest_count, rest_bytes });
        index.folders.insert(
            folder.path.clone(),
            IndexedFolder { rule: folder.rule.clone(), dev: folder.dev, ino: folder.ino, entries: indexed },
        );
    }
    index
}

fn state_of(
    child: &EntryResult,
    path: &Path,
    rule: &str,
    others: &[(PathBuf, bool)],
    protected: &[PathBuf],
    cx: &Context<'_>,
) -> EntryState {
    if child.error.is_some() {
        return EntryState::NotAnEntry(NotAnEntry::Unreadable);
    }
    match child.kind {
        EntryKind::Symlink => return EntryState::NotAnEntry(NotAnEntry::Symlink),
        EntryKind::Other => return EntryState::NotAnEntry(NotAnEntry::Other),
        EntryKind::Dir | EntryKind::File => {}
    }
    if child.flags & SF_DATALESS != 0 {
        return EntryState::NotAnEntry(NotAnEntry::Dataless);
    }
    if others.iter().any(|(p, _)| p == path) {
        return EntryState::NotAnEntry(NotAnEntry::Finding);
    }
    let kept = others.iter().any(|(p, kept)| *kept && stats::counted(Counter::AncestorChecks, || p.starts_with(path)))
        || cx.markers.iter().any(|m| stats::counted(Counter::AncestorChecks, || m.starts_with(path)))
        || cx.excludes.iter().any(|e| stats::counted(Counter::AncestorChecks, || e.starts_with(path)));
    let facts = EntryFacts {
        home: cx.home,
        rule,
        path,
        modified: child.modified,
        added: child.added,
        now: cx.now,
        kept,
        protected: protected.iter().any(|p| p.starts_with(path)),
        other_volume: child.other_fs > 0,
        finished: rule == BACKUP_RULE && child.kind == EntryKind::Dir && backups::finished(path),
    };
    match lock_of(&facts) {
        Some(lock) => EntryState::Locked(lock),
        None => EntryState::Pickable,
    }
}

/// The report's line for a listed entry: what only the listed ones need.
fn listed(path: PathBuf, child: &EntryResult, e: IndexedEntry, rule: &str, others: &[(PathBuf, bool)]) -> FolderEntry {
    let is_dir = e.is_dir;
    let backup = (rule == BACKUP_RULE && is_dir)
        .then(|| backups::info(&path))
        .map(|info| Backup { device_name: info.device_name, last_backup: info.last_backup.and_then(super::rfc3339) });
    FolderEntry {
        kind: if is_dir { FolderEntryKind::Folder } else { FolderEntryKind::File },
        bytes: e.bytes,
        dev: e.dev,
        ino: e.ino,
        modified: child.modified.and_then(rfc3339),
        lock: match e.state {
            EntryState::Locked(lock) => Some(lock),
            _ => None,
        },
        incomplete: e.incomplete,
        findings: others.iter().filter(|(p, _)| p != &path && p.starts_with(&path)).count() as u64,
        git: is_dir && fs::symlink_metadata(path.join(".git")).is_ok(),
        backup: backup.filter(|b| b.device_name.is_some() || b.last_backup.is_some()),
        path,
    }
}

fn rfc3339(secs: i64) -> Option<String> {
    super::rfc3339(UNIX_EPOCH.checked_add(Duration::from_secs(u64::try_from(secs).ok()?))?)
}

#[cfg(test)]
mod tests {
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
        };
        let path = home.join("Downloads").join(&child.name);
        state_of(child, &path, "user.downloads", &[], &[], &cx)
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
        broken.error = Some(libc::EACCES);
        assert_eq!(state(&broken), EntryState::NotAnEntry(NotAnEntry::Unreadable));
        assert_eq!(state(&child("p", EntryKind::Other)), EntryState::NotAnEntry(NotAnEntry::Other));
        assert_eq!(state(&child("l", EntryKind::Symlink)), EntryState::NotAnEntry(NotAnEntry::Symlink));
        let mut partly = child("d", EntryKind::Dir);
        partly.errors = 2;
        assert_eq!(state(&partly), EntryState::Pickable, "partly unreadable stays pickable");
    }
}
