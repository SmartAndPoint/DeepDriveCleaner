//! Re-check of every plan item right before it is touched (ADR §5): the
//! scan may be seconds or days old.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use super::{AgentLiveBy, ProcessTable, SkipReason};
use crate::agents::liveness::{
    self, FileFact, HarnessBy, HarnessLiveness, SessionFacts, SessionLiveness, UnknownCause,
};
use crate::agents::{CURSOR_AGENT, HarnessId, Installs, UnknownReason};
use crate::env::Env;
use crate::model::NotSelected;
use crate::plan::{Action, PlanItem};
use crate::platform::PLATFORM;
use crate::platform::fsops::DirFd;
use crate::platform::procs::ProcInfo;
use crate::platform::types::EntryStat;
use crate::report::backups;
use crate::report::contents::CLAUDE_RECORDS;
use crate::rules::entries::{self, BACKUP_RULE, EntryFacts, EntryShape, SessionPart, lock_of, session_part};
use crate::rules::{Catalog, KEEP_MARKER, owns, protected_in_home, protected_system};
use crate::scan::reader::SF_DATALESS;
use crate::scan::{self, DirCtx, DirReader, EntryMeta, OwnerKind, Progress, SystemReader, Tag, Visitor, WalkOptions};
use crate::stats;

/// A verified target: its parent is open and checked; act through it.
#[derive(Debug)]
pub struct Verified {
    pub parent: DirFd,
    pub name: OsString,
    /// `(dev, ino)` of the finding as checked.
    pub id: (u64, u64),
    /// A directory as checked; a picked entry may be a regular file.
    pub is_dir: bool,
    /// A session's entry: its companions, moved before it.
    pub session: Option<SessionParts>,
}

/// A session entry's parts beside its key, as the guard found them under the
/// entry's parent descriptor (ADR "Agent footprint: worktrees, harness data
/// and bounded consent for agents", §3): the companions are moved first,
/// each with a journal line of its own, and the key last.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionParts {
    /// The key's own bytes: the transcript, or the session's folder.
    pub key_bytes: u64,
    /// By name.
    pub companions: Vec<Companion>,
}

/// One companion of a session's entry, as checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Companion {
    pub name: OsString,
    pub id: (u64, u64),
    pub is_dir: bool,
    pub bytes: u64,
}

/// What the guard derives again of a session's entry, its key and its
/// companions together.
pub(super) struct SessionCheck {
    pub(super) parts: SessionParts,
    pub(super) kept: bool,
    pub(super) protected: bool,
    pub(super) other_volume: bool,
    /// The latest change of the key and the companions.
    pub(super) modified: Option<i64>,
    pub(super) liveness: SessionLiveness,
}

impl Verified {
    /// The same directory or file is still at the same place (through the parent fd).
    pub fn unchanged(&self) -> bool {
        self.parent.stat_child(&self.name).is_ok_and(|st| {
            !st.is_symlink && st.is_dir == self.is_dir && (st.is_dir || st.is_file) && (st.dev, st.ino) == self.id
        })
    }
}

pub struct Guard<'a> {
    catalog: &'a Catalog,
    home: PathBuf,
    /// The run's start, against which an entry is "recent".
    now: SystemTime,
    roots: Vec<PathBuf>,
    protected_paths: HashSet<String>,
    protected_ids: HashSet<(u64, u64)>,
    /// Directories containing a keep marker inside planned findings.
    markers: Vec<PathBuf>,
    /// Per process inside an app bundle: its pid, the bundle name, and the
    /// executable's own name, which still names a renamed bundle (`Chrome.app`
    /// runs `Google Chrome`, `Xcode16.app` runs `Xcode`).
    /// Processes of the snapshot the check could not read in full.
    withheld: u64,
    apps: Vec<(i32, String, String)>,
    /// Per process: absolute paths it uses, and first components of relative
    /// paths whose base (cwd) is unknown.
    uses: Vec<Uses>,
    /// Executable names in the snapshot, for the dry run's runtime check.
    names: HashSet<String>,
    /// For the confirming re-read of one process, the fresh read before a
    /// tool's command, and each liveness read.
    procs: &'a dyn ProcessTable,
    /// The folders the scan never opens: Claude Code's records under one
    /// are not read, and every session is then `liveness-unknown`.
    skips: Vec<PathBuf>,
    /// For the walk of a session's folders.
    reader: &'a dyn DirReader,
}

struct Uses {
    pid: i32,
    /// The app it belongs to, else its own name.
    name: String,
    paths: Vec<PathBuf>,
    unresolved: Vec<OsString>,
    /// Files it has open, as the kernel reports them.
    open: Vec<PathBuf>,
}

/// What the in-use check looks at, by action (ADR §5, amendment of 2026-10-20
/// for 0.2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UseCheck {
    /// `native-only`: the runtime's daemon lives inside the finding by design
    /// (Docker Desktop's backend works there), and its prune commands are what
    /// knows what is in use.
    Exempt,
    /// A move to the Trash: a file open there keeps working.
    Paths,
    /// Deletion, and a tool's command with a fallback: open files count too
    /// (a running `uv run` holds uv's cache lock).
    PathsAndOpenFiles,
}

fn use_check(action: &Action) -> UseCheck {
    match action {
        Action::NativeOnly(_) => UseCheck::Exempt,
        Action::Trash | Action::TrashEntry => UseCheck::Paths,
        Action::Delete { .. } | Action::Native { .. } => UseCheck::PathsAndOpenFiles,
    }
}

/// How long an open-file hit waits for its confirming re-read: a passing
/// reader such as a Spotlight worker lets go by then.
const OPEN_FILE_CONFIRM: Duration = Duration::from_millis(250);

impl<'a> Guard<'a> {
    pub fn new(
        env: &Env,
        catalog: &'a Catalog,
        roots: &[PathBuf],
        markers: Vec<PathBuf>,
        procs: &'a dyn ProcessTable,
    ) -> Self {
        let table = procs.processes();
        let mut paths: Vec<PathBuf> = protected_in_home(PLATFORM).iter().map(|rel| env.home().join(rel)).collect();
        paths.extend(protected_system(PLATFORM).iter().map(PathBuf::from));
        paths.extend(roots.iter().cloned());
        let protected_ids = paths
            .iter()
            .filter_map(|p| fs::symlink_metadata(p).ok())
            .filter_map(|m| crate::platform::meta::file_id(&m).ok())
            .collect();
        Self {
            catalog,
            home: env.home().to_path_buf(),
            now: env.now(),
            roots: roots.to_vec(),
            protected_paths: paths.iter().map(|p| fold(p)).collect(),
            protected_ids,
            markers,
            uses: table.iter().map(uses).collect(),
            names: table.iter().map(|p| p.name.clone()).collect(),
            apps: table.iter().filter_map(|p| Some((p.pid, p.app_bundle()?, p.name.clone()))).collect(),
            withheld: table.iter().filter(|p| p.withheld).count() as u64,
            procs,
            skips: Vec::new(),
            reader: &SystemReader,
        }
    }

    /// The scan's skips (`ScanOptions::skip` and `user_skips`).
    pub fn with_skips(mut self, skips: Vec<PathBuf>) -> Self {
        self.skips = skips;
        self
    }

    /// The reader the walks of a session's folders take.
    pub fn with_reader(mut self, reader: &'a dyn DirReader) -> Self {
        self.reader = reader;
        self
    }

    /// Processes of the snapshot whose details the check could not read in
    /// full: what they use is unknown (safety ADR, §5, amendment of 2026-10-31).
    pub fn withheld(&self) -> u64 {
        self.withheld
    }

    /// A process named `name` was running at the snapshot.
    pub fn running(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    /// Right before a tool's command: the open files read afresh, since a
    /// long clean reaches the item minutes after the snapshot.
    pub fn before_tool(&self, path: &Path) -> Option<SkipReason> {
        let fresh: Vec<Uses> = self
            .procs
            .processes()
            .into_iter()
            .filter(|p| !p.open_files.is_empty())
            .map(|p| Uses {
                pid: p.pid,
                name: p.app_bundle().unwrap_or_else(|| p.name.clone()),
                paths: Vec::new(),
                unresolved: Vec::new(),
                open: p.open_files,
            })
            .collect();
        self.holding(path, &fresh)
    }

    fn in_use(&self, path: &Path, check: UseCheck) -> Option<SkipReason> {
        match check {
            UseCheck::Exempt => None,
            UseCheck::Paths => in_use(path, &self.uses),
            UseCheck::PathsAndOpenFiles => in_use(path, &self.uses).or_else(|| self.holding(path, &self.uses)),
        }
    }

    /// The first process with a file open inside `target` that still has one
    /// after [`OPEN_FILE_CONFIRM`]; one wait for all of them.
    fn holding(&self, target: &Path, uses: &[Uses]) -> Option<SkipReason> {
        let holders: Vec<&Uses> = uses.iter().filter(|u| u.open.iter().any(|f| f.starts_with(target))).collect();
        if holders.is_empty() {
            return None;
        }
        std::thread::sleep(OPEN_FILE_CONFIRM);
        holders
            .into_iter()
            .find(|u| self.procs.open_files(u.pid).iter().any(|f| f.starts_with(target)))
            .map(|u| SkipReason::InUse { pid: u.pid, name: u.name.clone() })
    }

    pub fn check(&self, item: &PlanItem) -> Result<Verified, SkipReason> {
        if item.action == Action::TrashEntry {
            return self.check_entry(item);
        }
        let f = &item.finding;
        let path = f.path.as_path();
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(SkipReason::Protected);
        };
        if !self.roots.iter().any(|r| path.starts_with(r)) {
            return Err(SkipReason::OutsideScanRoots);
        }
        if self.roots.iter().any(|r| r == path) {
            return Err(SkipReason::ScanRoot);
        }
        if f.incomplete {
            // Unreadable parts or another filesystem mounted inside: never delete blind.
            return Err(SkipReason::Incomplete);
        }
        if f.not_selected == Some(NotSelected::ForeignOwner) {
            // Another user's entries: the rename would succeed and the removal fail halfway.
            return Err(SkipReason::ForeignOwner);
        }
        if self.protected_paths.contains(&fold(path)) {
            return Err(SkipReason::Protected);
        }
        // No symlink anywhere above: the canonical parent is the parent we scanned.
        if parent.canonicalize().ok().as_deref() != Some(parent) {
            return Err(SkipReason::ParentChanged);
        }
        let parent_fd = DirFd::open(parent).map_err(|_| SkipReason::ParentChanged)?;
        let parent_id = parent_fd.identity().map_err(|_| SkipReason::ParentChanged)?;
        if f.parent_dev.zip(f.parent_ino).is_some_and(|id| id != parent_id) {
            return Err(SkipReason::ParentChanged);
        }
        let st = match parent_fd.stat_child(name) {
            Ok(st) => st,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(SkipReason::Missing),
            Err(_) => return Err(SkipReason::ChangedSinceScan),
        };
        if st.is_symlink {
            return Err(SkipReason::Symlink);
        }
        if !st.is_dir {
            return Err(SkipReason::NotADirectory);
        }
        if (st.dev, st.ino) != (f.dev, f.ino) {
            return Err(SkipReason::ChangedSinceScan);
        }
        if self.protected_ids.contains(&(st.dev, st.ino)) {
            return Err(SkipReason::Protected);
        }
        match self.catalog.by_id(&f.rule) {
            Some(rule) if self.catalog.verify(rule, path) => {}
            _ => return Err(SkipReason::RuleMismatch),
        }
        if fs::symlink_metadata(path.join(".git")).is_ok() || f.project.as_ref().is_some_and(|p| p.root == path) {
            return Err(SkipReason::ProjectRoot);
        }
        if self.kept(path) {
            return Err(SkipReason::Kept);
        }
        if let Some(rule) = self.catalog.by_id(&f.rule)
            && let Some((pid, app, _)) =
                self.apps.iter().find(|(_, app, exe)| rule.owners.iter().any(|o| owns(o, app) || owns(o, exe)))
        {
            return Err(SkipReason::OwnerRunning { pid: *pid, name: app.clone() });
        }
        // Read afresh for each item, Codex's locks too: a harness started
        // since the plan holds its folder back.
        if let Some(harness) = liveness::rule_harness(&f.rule) {
            let table = self.procs.processes();
            if let Some(reason) =
                agent_live(harness, harness_state(&self.home, harness, &table, self.procs, &self.skips))
            {
                return Err(reason);
            }
        }
        if let Some(reason) = self.in_use(path, use_check(&item.action)) {
            return Err(reason);
        }
        Ok(Verified { parent: parent_fd, name: name.to_os_string(), id: (st.dev, st.ino), is_dir: true, session: None })
    }

    /// §5 for a picked entry (safety ADR, amendment of 2026-10-20): the
    /// parent is the folder, the entry a regular file or a directory with the
    /// listed identity. The rule-marker match applies to the parent; the
    /// `incomplete`, project-root and directory-only checks do not apply. A
    /// keep marker in or at the entry is its lock, "kept".
    fn check_entry(&self, item: &PlanItem) -> Result<Verified, SkipReason> {
        let f = &item.finding;
        let path = f.path.as_path();
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(SkipReason::Protected);
        };
        if !self.roots.iter().any(|r| path.starts_with(r)) {
            return Err(SkipReason::OutsideScanRoots);
        }
        if self.roots.iter().any(|r| r == path) {
            return Err(SkipReason::ScanRoot);
        }
        if self.protected_paths.contains(&fold(path)) {
            return Err(SkipReason::Protected);
        }
        if parent.canonicalize().ok().as_deref() != Some(parent) {
            return Err(SkipReason::ParentChanged);
        }
        let parent_fd = DirFd::open(parent).map_err(|_| SkipReason::ParentChanged)?;
        let parent_id = parent_fd.identity().map_err(|_| SkipReason::ParentChanged)?;
        if f.parent_dev.zip(f.parent_ino).is_some_and(|id| id != parent_id) {
            return Err(SkipReason::ParentChanged);
        }
        match self.catalog.by_id(&f.rule) {
            Some(rule) if self.catalog.verify(rule, parent) => {}
            _ => return Err(SkipReason::NotAnEntry),
        }
        let Some(policy) = entries::policy(&f.rule) else { return Err(SkipReason::NotAnEntry) };
        let st = match parent_fd.stat_child(name) {
            Ok(st) => st,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(SkipReason::Missing),
            Err(_) => return Err(SkipReason::ChangedSinceScan),
        };
        if st.is_symlink {
            return Err(SkipReason::Symlink);
        }
        if !st.is_dir && !st.is_file {
            return Err(SkipReason::NotAnEntry);
        }
        if (st.dev, st.ino) != (f.dev, f.ino) {
            return Err(SkipReason::ChangedSinceScan);
        }
        if self.protected_ids.contains(&(st.dev, st.ino)) {
            return Err(SkipReason::Protected);
        }
        // A session's entry is keyed by its transcript or its folder; its
        // companions are found again here, under the same descriptor.
        let session = match policy.shape {
            EntryShape::Child => None,
            shape => match session_part(shape, name.as_encoded_bytes(), st.is_dir) {
                SessionPart::Key(session) => Some(self.session_check(path, &parent_fd, shape, session, &st)?),
                SessionPart::Companion(_) | SessionPart::None => return Err(SkipReason::NotAnEntry),
            },
        };
        // The plan is not trusted input: the entry's lock is derived again,
        // as the scan and the dry run derive it. A volume mounted deeper is
        // the scan's to see; one mounted at the entry is seen here.
        let (modified, added) = parent_fd.child_times(name).unwrap_or((None, None));
        let rule = f.rule.as_str();
        let facts = EntryFacts {
            home: &self.home,
            rule,
            path,
            modified: session.as_ref().map_or(modified, |s| s.modified),
            added,
            now: self.now,
            kept: self.markers.iter().any(|m| m.starts_with(path))
                || fs::symlink_metadata(path.join(KEEP_MARKER)).is_ok()
                || session.as_ref().is_some_and(|s| s.kept),
            protected: protected_in_home(PLATFORM).iter().any(|rel| self.home.join(rel).starts_with(path))
                || session.as_ref().is_some_and(|s| s.protected),
            other_volume: st.dev != parent_id.0 || session.as_ref().is_some_and(|s| s.other_volume),
            session: session.as_ref().map(|s| s.liveness),
            finished: rule == BACKUP_RULE && st.is_dir && backups::finished(path),
        };
        if let Some(lock) = lock_of(&facts) {
            return Err(SkipReason::EntryLocked { lock });
        }
        // Above it only: the markers walked inside other listed entries are theirs.
        if self.marker_above(path) {
            return Err(SkipReason::Kept);
        }
        if let Some(reason) = self.in_use(path, UseCheck::Paths) {
            return Err(reason);
        }
        let parts = session.map(|s| s.parts);
        for companion in parts.iter().flat_map(|p| &p.companions) {
            if let Some(reason) = self.in_use(&parent.join(&companion.name), UseCheck::Paths) {
                return Err(reason);
            }
        }
        Ok(Verified {
            parent: parent_fd,
            name: name.to_os_string(),
            id: (st.dev, st.ino),
            is_dir: st.is_dir,
            session: parts,
        })
    }

    /// A session's entry beside its key `path`: its companions, listed again
    /// and each `stat`ed through `parent_fd`, by name; the bytes, keep
    /// markers and mounts of the key and of each; and the session's
    /// liveness, read now. A companion gone since the listing is left out;
    /// one that cannot move as it is (a symlink, a dataless file, neither a
    /// file nor a folder) leaves the entry `incomplete`, as a folder that
    /// cannot be read through.
    pub(super) fn session_check(
        &self,
        path: &Path,
        parent_fd: &DirFd,
        shape: EntryShape,
        session: &str,
        key: &EntryStat,
    ) -> Result<SessionCheck, SkipReason> {
        let (Some(parent), Some(key_name)) = (path.parent(), path.file_name()) else {
            return Err(SkipReason::NotAnEntry);
        };
        let mut names: Vec<OsString> = Vec::new();
        for child in stats::read_dir(parent).map_err(|_| SkipReason::ChangedSinceScan)? {
            let name = child.map_err(|_| SkipReason::ChangedSinceScan)?.file_name();
            if name != key_name
                && session_part(shape, name.as_encoded_bytes(), false) == SessionPart::Companion(session)
            {
                names.push(name);
            }
        }
        names.sort();
        let parent_dev = parent_fd.identity().map_err(|_| SkipReason::ParentChanged)?.0;
        let (key_bytes, key_walk) = self.measure(path, key)?;
        let key_modified = parent_fd.child_times(key_name).ok().and_then(|(modified, _)| modified);
        let mut check = SessionCheck {
            parts: SessionParts { key_bytes, companions: Vec::new() },
            kept: key_walk.kept,
            protected: false,
            other_volume: key_walk.other_fs,
            modified: key_modified,
            liveness: SessionLiveness::NotLive,
        };
        let mut files = vec![FileFact { id: (key.dev, key.ino), modified: key_modified }];
        for name in names {
            let st = match parent_fd.stat_child(&name) {
                Ok(st) => st,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => return Err(SkipReason::ChangedSinceScan),
            };
            if st.is_symlink || !(st.is_dir || st.is_file) || st.flags & SF_DATALESS != 0 {
                return Err(SkipReason::Incomplete);
            }
            let at = parent.join(&name);
            let (bytes, walked) = self.measure(&at, &st)?;
            let modified = parent_fd.child_times(&name).ok().and_then(|(modified, _)| modified);
            check.kept |= walked.kept
                || self.markers.iter().any(|m| m.starts_with(&at))
                || (st.is_dir && fs::symlink_metadata(at.join(KEEP_MARKER)).is_ok());
            check.protected |= self.protected_ids.contains(&(st.dev, st.ino))
                || protected_in_home(PLATFORM).iter().any(|rel| self.home.join(rel).starts_with(&at));
            check.other_volume |= walked.other_fs || st.dev != parent_dev;
            check.modified = check.modified.max(modified);
            files.push(FileFact { id: (st.dev, st.ino), modified });
            check.parts.companions.push(Companion { name, id: (st.dev, st.ino), is_dir: st.is_dir, bytes });
        }
        check.liveness = self.session_liveness(session, files);
        Ok(check)
    }

    /// `path`'s bytes, and for a folder what its walk saw: a file is its
    /// allocation, a folder that cannot be read through is `incomplete`.
    fn measure(&self, path: &Path, st: &EntryStat) -> Result<(u64, Walked), SkipReason> {
        if !st.is_dir {
            return Ok((st.alloc, Walked::default()));
        }
        let walked = walk_item(path, self.reader).ok_or(SkipReason::Incomplete)?;
        if walked.incomplete && !walked.other_fs {
            return Err(SkipReason::Incomplete);
        }
        Ok((walked.bytes, walked))
    }

    /// Claude Code's check for one session, read now: its records (none
    /// under a skipped folder, which leaves it `liveness-unknown`, as the
    /// scan does), the user's processes and the files they hold open.
    fn session_liveness(&self, session: &str, files: Vec<FileFact>) -> SessionLiveness {
        let dir = self.home.join(CLAUDE_RECORDS);
        let records = if self.skips.iter().any(|s| dir.starts_with(s)) {
            Err(UnknownCause::RecordsSkipped)
        } else {
            liveness::claude_records(&dir, |pid| self.procs.process_start(pid))
        };
        let pids = match &records {
            Ok(_) => self.procs.pids(),
            Err(_) => Vec::new(),
        };
        let facts = [SessionFacts { session: session.to_owned(), files }];
        liveness::claude(records.as_deref().map_err(|c| *c), &facts, &pids, |pid| self.procs.open_vnodes(pid), self.now)
            .pop()
            .unwrap_or(SessionLiveness::Unknown { cause: UnknownCause::SessionWithoutFiles })
    }

    fn kept(&self, path: &Path) -> bool {
        self.markers.iter().any(|m| m.starts_with(path))
            || fs::symlink_metadata(path.join(KEEP_MARKER)).is_ok()
            || self.marker_above(path)
    }

    /// A keep marker in a directory above `path`, within its scan root.
    pub(super) fn marker_above(&self, path: &Path) -> bool {
        let root = self.roots.iter().filter(|r| path.starts_with(r)).max_by_key(|r| r.as_os_str().len());
        path.ancestors()
            .skip(1)
            .take_while(|a| root.is_none_or(|r| a.starts_with(r)))
            .any(|a| fs::symlink_metadata(a.join(KEEP_MARKER)).is_ok())
    }
}

/// Whether `harness` is live: `table` is the process table the caller read,
/// and Codex's thread locks are read here, unless a skip covers them (then
/// Codex is unknown, as `ddc disk agents` says).
pub(crate) fn harness_state(
    home: &Path,
    harness: HarnessId,
    table: &[ProcInfo],
    procs: &dyn ProcessTable,
    skips: &[PathBuf],
) -> HarnessLiveness {
    let packages = home.join(".codex/packages");
    let cursor_agent = home.join(CURSOR_AGENT);
    let installs = Installs { codex_packages: &packages, cursor_agent: &cursor_agent };
    let locks = if harness == HarnessId::Codex {
        let dir = home.join(".codex/thread-writer-locks");
        if skips.iter().any(|s| dir.starts_with(s)) { Err(UnknownReason::Skipped) } else { liveness::codex_locks(&dir) }
    } else {
        Err(UnknownReason::Missing)
    };
    liveness::harness(harness, table, installs, &locks, |pid| procs.open_vnodes(pid))
}

/// The skip of a folder whose harness is not known to be idle.
pub(crate) fn agent_live(harness: HarnessId, state: HarnessLiveness) -> Option<SkipReason> {
    let by = match state {
        HarnessLiveness::NotLive => return None,
        HarnessLiveness::Live { by: HarnessBy::Running } => AgentLiveBy::Running,
        HarnessLiveness::Live { by: HarnessBy::LockHeld } => AgentLiveBy::LockHeld,
        HarnessLiveness::Unknown { .. } => AgentLiveBy::Unknown,
    };
    Some(SkipReason::AgentLive { harness, by })
}

#[derive(Debug, Default)]
pub(super) struct Walked {
    pub(super) bytes: u64,
    pub(super) kept: bool,
    pub(super) incomplete: bool,
    /// Another filesystem is mounted inside.
    pub(super) other_fs: bool,
}

/// Walks one folder alone: its size, a `.ddc-keep` inside, unreadable parts.
pub(super) fn walk_item(path: &Path, reader: &dyn DirReader) -> Option<Walked> {
    struct Markers(Mutex<bool>);
    impl Visitor for Markers {
        fn wants_private(&self, _: &DirCtx<'_>, _: Tag) -> bool {
            false
        }
        fn on_file(&self, _: &Path, _: (u64, u64), entry: &EntryMeta, _: Option<(OwnerKind, Tag)>) {
            if entry.name == KEEP_MARKER {
                *self.0.lock().unwrap_or_else(|p| p.into_inner()) = true;
            }
        }
    }
    let markers = Markers(Mutex::new(false));
    let opts = WalkOptions { threads: Some(2), ..WalkOptions::default() };
    let out = scan::walk_with_reader(
        std::slice::from_ref(&path.to_path_buf()),
        &markers,
        &opts,
        &Progress::default(),
        reader,
    )
    .ok()?;
    let kept = markers.0.into_inner().unwrap_or_else(|p| p.into_inner());
    Some(Walked {
        bytes: out.total_bytes,
        kept,
        incomplete: out.errors_total > 0 || out.skipped_total > 0 || out.vanished > 0,
        other_fs: out.skipped.iter().any(|s| matches!(s.reason, scan::SkipReason::OtherFilesystem)),
    })
}

/// APFS is case-insensitive by default: compare protected paths folded.
fn fold(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}

/// What a process uses: its executable path (as passed to execve), path-like
/// arguments, working directory and `VIRTUAL_ENV`/`CONDA_PREFIX`, and apart
/// from them its open files, which are never `stat`ed (a protected folder could
/// prompt). Relative paths are joined with the process's cwd; computed once per
/// process. It is named by its app when it lives in an app bundle.
fn uses(p: &ProcInfo) -> Uses {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut unresolved = Vec::new();
    for raw in p.exe.iter().map(|e| e.as_os_str()).chain(p.args.iter().map(|a| a.as_os_str())) {
        let path = Path::new(raw);
        if path.is_absolute() {
            paths.push(lexical(path));
        } else if raw.to_string_lossy().contains('/') {
            match &p.cwd {
                Some(cwd) => paths.push(lexical(&cwd.join(path))),
                None => unresolved.extend(
                    path.components().find(|c| matches!(c, Component::Normal(_))).map(|c| c.as_os_str().to_os_string()),
                ),
            }
        }
    }
    paths.extend(p.cwd.iter().cloned());
    paths.extend(p.env_prefixes.iter().cloned());
    let canonical: Vec<PathBuf> = paths.iter().filter_map(|c| canonical_parent(c)).collect();
    paths.extend(canonical);
    let name = p.app_bundle().unwrap_or_else(|| p.name.clone());
    Uses { pid: p.pid, name, paths, unresolved, open: p.open_files.clone() }
}

/// The first running process that uses one of `targets` (spellings of one
/// folder), as a clean's in-use check counts it for a tool's command: a
/// path of it inside (its executable, an argument, its cwd, `VIRTUAL_ENV`,
/// `CONDA_PREFIX`), or a file it holds open there. For an area action,
/// outside a clean (story "Whole disk: Conda environments and its package
/// cache, with conda"). The process table is read once: reading every
/// process's open files is the cost.
pub(crate) fn user_of(targets: &[&Path], procs: &dyn ProcessTable) -> Option<SkipReason> {
    let uses: Vec<Uses> = procs.processes().iter().map(uses).collect();
    targets.iter().find_map(|target| {
        in_use(target, &uses).or_else(|| {
            uses.iter()
                .find(|u| u.open.iter().any(|f| f.starts_with(target)))
                .map(|u| SkipReason::InUse { pid: u.pid, name: u.name.clone() })
        })
    })
}

/// A process uses `target` if any of its paths lies inside it; with an
/// unknown cwd, a relative path starting with the target's name counts too.
fn in_use(target: &Path, uses: &[Uses]) -> Option<SkipReason> {
    let name = target.file_name()?;
    for u in uses {
        if u.paths.iter().any(|p| p.starts_with(target)) {
            return Some(SkipReason::InUse { pid: u.pid, name: u.name.clone() });
        }
        if u.unresolved.iter().any(|c| c == name) {
            return Some(SkipReason::InUseUnknownCwd { pid: u.pid, name: u.name.clone() });
        }
    }
    None
}

/// Removes `.` components (Path::components keeps a leading one).
fn lexical(p: &Path) -> PathBuf {
    p.components().filter(|c| !matches!(c, Component::CurDir)).collect()
}

/// `/tmp/x/.venv/bin/python` → `/private/tmp/x/.venv/bin/python`: resolve the
/// parent, keep the last component unresolved (it may be a symlink out of the venv).
fn canonical_parent(p: &Path) -> Option<PathBuf> {
    Some(p.parent()?.canonicalize().ok()?.join(p.file_name()?))
}
