//! Whether a coding agent still uses its data (ADR "Agent footprint:
//! worktrees, harness data and bounded consent for agents", §3, Liveness):
//! per session for Claude Code, per harness for Codex, opencode and Cursor.
//!
//! The decisions are pure: they take the facts (Claude Code's records as
//! [`claude_records`] reads them, the identities and times of a session's
//! files, the user's processes and the files each holds open, Codex's
//! thread locks) and read nothing themselves, so a test injects every
//! fact, and the scan, the plan and the guard decide alike. Nothing here
//! removes or moves anything.

use std::collections::HashSet;
use std::path::Path;
use std::time::SystemTime;

use crate::exec::ProcessTable;
use crate::platform::procs::{self, OpenVnode, ProcInfo};
use crate::rules::entries;

use super::{HarnessId, RunningKind, UnknownReason, claude, codex, running};

/// One Claude Code record (`sessions/<pid>.json`), decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordOutcome {
    /// Its process is the user's and the one that wrote it; `session` is
    /// its `sessionId`, absent when the record names none.
    Live { session: Option<String> },
    /// Its process is gone, or the pid now belongs to another process: a
    /// crash leftover, which does not block.
    Gone,
    /// It lacks a pid or `startedAt`, or names a process of another pid
    /// domain.
    Undecidable { session: Option<String> },
    /// It could not be read as a record.
    Unparsable,
}

/// Why a session's liveness could not be told. Declared from the widest
/// to the narrowest: when several apply, the first is reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum UnknownCause {
    /// The records folder does not exist: an older Claude Code, or a
    /// changed layout.
    RecordsMissing,
    /// The records folder, or an entry of it, could not be read.
    RecordsUnreadable,
    /// A record could not be parsed.
    UnparsableRecord,
    /// A record could not be decided and names no session.
    UndecidableRecord,
    /// A live record names no session.
    LiveRecordWithoutSession,
    /// A record that could not be decided names this session.
    UndecidableSessionRecord,
}

/// What shows a session live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LiveBy {
    /// A live record names it.
    Record,
    /// Its transcript or a companion changed within [`entries::RECENT`].
    Recent,
    /// A process of the user holds one of its files open.
    OpenFile,
}

/// A session's state, as the plan and the guard see it: only `NotLive`
/// lets its files go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionLiveness {
    Live { by: LiveBy },
    Unknown { cause: UnknownCause },
    NotLive,
}

/// One file of a session, its transcript or a companion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileFact {
    /// `(dev, ino)`, as `platform::meta::file_id` gives it.
    pub id: (u64, u64),
    /// Its own modification time, seconds since the epoch; `None` when not
    /// known, which counts as recent.
    pub modified: Option<i64>,
}

/// A Claude Code session: its id and its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFacts {
    pub session: String,
    pub files: Vec<FileFact>,
}

/// Claude Code's records under `dir` (`~/.claude/sessions`), each decided
/// against `table` for this user: the facts [`claude()`] takes.
pub fn claude_records(dir: &Path, table: &dyn ProcessTable) -> Result<Vec<RecordOutcome>, UnknownCause> {
    claude::records(dir, |pid| table.process_start(pid), procs::uid())
}

/// Each of `sessions`' state, in their order. `records` is [`claude_records`]'
/// answer; `pids` are the user's processes, and `open` gives the files one
/// holds open; `now` is the time the check runs.
///
/// Every session is unknown when the records could not be listed, when any
/// record could not be parsed, when a record could not be decided and
/// names no session, or when a live record names no session. Otherwise a
/// session is live when a live record names it, when one of its files
/// changed within [`entries::RECENT`] of `now`, or when one of its files
/// is open; it is unknown when an undecidable record names it. The cheaper
/// answers come first, and the open files are swept only for the sessions
/// still undecided.
pub fn claude(
    records: Result<&[RecordOutcome], UnknownCause>,
    sessions: &[SessionFacts],
    pids: &[i32],
    open: impl Fn(i32) -> Vec<OpenVnode>,
    now: SystemTime,
) -> Vec<SessionLiveness> {
    let (live, undecided) = match named(records) {
        Ok(named) => named,
        Err(cause) => return vec![SessionLiveness::Unknown { cause }; sessions.len()],
    };
    let mut out: Vec<Option<SessionLiveness>> = sessions
        .iter()
        .map(|s| {
            if live.contains(s.session.as_str()) {
                Some(SessionLiveness::Live { by: LiveBy::Record })
            } else if s.files.iter().any(|f| entries::recent(f.modified, None, now)) {
                Some(SessionLiveness::Live { by: LiveBy::Recent })
            } else if undecided.contains(s.session.as_str()) {
                Some(SessionLiveness::Unknown { cause: UnknownCause::UndecidableSessionRecord })
            } else {
                None
            }
        })
        .collect();
    let ids: HashSet<(u64, u64)> = sessions
        .iter()
        .zip(&out)
        .filter(|(_, o)| o.is_none())
        .flat_map(|(s, _)| s.files.iter().map(|f| f.id))
        .collect();
    let held = if ids.is_empty() { HashSet::new() } else { codex::held(&ids, pids, open) };
    for (s, o) in sessions.iter().zip(&mut out) {
        if o.is_none() && s.files.iter().any(|f| held.contains(&f.id)) {
            *o = Some(SessionLiveness::Live { by: LiveBy::OpenFile });
        }
    }
    out.into_iter().map(|o| o.unwrap_or(SessionLiveness::NotLive)).collect()
}

/// The sessions live records name, and those undecidable records name; or
/// why no session can be told.
fn named(records: Result<&[RecordOutcome], UnknownCause>) -> Result<(HashSet<&str>, HashSet<&str>), UnknownCause> {
    let records = records?;
    let mut live = HashSet::new();
    let mut undecided = HashSet::new();
    let mut cause: Option<UnknownCause> = None;
    for record in records {
        let blocks = match record {
            RecordOutcome::Gone => None,
            RecordOutcome::Unparsable => Some(UnknownCause::UnparsableRecord),
            RecordOutcome::Undecidable { session: None } => Some(UnknownCause::UndecidableRecord),
            RecordOutcome::Live { session: None } => Some(UnknownCause::LiveRecordWithoutSession),
            RecordOutcome::Live { session: Some(s) } => {
                live.insert(s.as_str());
                None
            }
            RecordOutcome::Undecidable { session: Some(s) } => {
                undecided.insert(s.as_str());
                None
            }
        };
        if let Some(b) = blocks {
            cause = Some(cause.map_or(b, |c| c.min(b)));
        }
    }
    match cause {
        Some(cause) => Err(cause),
        None => Ok((live, undecided)),
    }
}

/// Codex's thread locks as `thread-writer-locks` lists them: each thread
/// id with its lock's `(dev, ino)`, or why the folder could not be listed.
pub type CodexLocks = Result<Vec<(String, (u64, u64))>, UnknownReason>;

/// What shows a harness running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HarnessBy {
    /// A process of the harness runs.
    Running,
    /// A process of the user holds a Codex thread lock open.
    LockHeld,
}

/// Why it could not be told whether a harness runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HarnessCause {
    /// A binary of the harness runs that ddc cannot name, such as Codex
    /// inside another app's bundle.
    UnnamedProcess,
    /// Codex's `thread-writer-locks` folder could not be read.
    LocksUnreadable,
}

/// A harness's state: only `NotLive` lets its data go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HarnessLiveness {
    Live { by: HarnessBy },
    Unknown { cause: HarnessCause },
    NotLive,
}

/// Whether `harness` runs. `procs` are the user's processes and `open`
/// gives the files one holds open; `codex_packages` is
/// `~/.codex/packages`. `codex_locks` is what `thread-writer-locks` lists
/// (read only for Codex): a missing folder holds no lock, and an
/// unreadable one leaves Codex unknown unless a process decides first.
///
/// Codex runs when its CLI or app-server runs or a lock is held open;
/// opencode when an `opencode` runs; Cursor when the app or
/// `cursor-agent` runs. A process of the harness that ddc cannot name is
/// unknown. Claude Code's data is decided per session ([`claude()`]); here
/// it is only whether a `claude` runs.
pub fn harness(
    harness: HarnessId,
    procs: &[ProcInfo],
    codex_packages: &Path,
    codex_locks: &CodexLocks,
    open: impl Fn(i32) -> Vec<OpenVnode>,
) -> HarnessLiveness {
    let runs = running::of(harness, procs, codex_packages);
    if runs.iter().any(|r| r.kind != RunningKind::Other) {
        return HarnessLiveness::Live { by: HarnessBy::Running };
    }
    let mut cause = runs.iter().any(|r| r.kind == RunningKind::Other).then_some(HarnessCause::UnnamedProcess);
    if harness == HarnessId::Codex {
        match codex_locks {
            Ok(locks) if !locks.is_empty() => {
                let ids: HashSet<(u64, u64)> = locks.iter().map(|(_, id)| *id).collect();
                let pids: Vec<i32> = procs.iter().map(|p| p.pid).collect();
                if !codex::held(&ids, &pids, open).is_empty() {
                    return HarnessLiveness::Live { by: HarnessBy::LockHeld };
                }
            }
            Ok(_) | Err(UnknownReason::Missing) => {}
            Err(_) => cause = cause.or(Some(HarnessCause::LocksUnreadable)),
        }
    }
    match cause {
        Some(cause) => HarnessLiveness::Unknown { cause },
        None => HarnessLiveness::NotLive,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    // Fake pids and times only: never the test's own process.
    const NOW: u64 = 1_793_180_467;
    const OLD: i64 = NOW as i64 - 3600;
    const PACKAGES: &str = "/Users/example/.codex/packages";

    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(NOW)
    }

    fn live(s: &str) -> RecordOutcome {
        RecordOutcome::Live { session: Some(s.to_owned()) }
    }

    fn undecidable(s: &str) -> RecordOutcome {
        RecordOutcome::Undecidable { session: Some(s.to_owned()) }
    }

    /// A session of one transcript, `(1, ino)`, changed at `modified`.
    fn session(id: &str, ino: u64, modified: i64) -> SessionFacts {
        SessionFacts { session: id.to_owned(), files: vec![FileFact { id: (1, ino), modified: Some(modified) }] }
    }

    /// Fake pid 4242 holds `(1, 30)` open.
    fn open(pid: i32) -> Vec<OpenVnode> {
        if pid == 4242 { vec![OpenVnode { dev: 1, ino: 30, path: None }] } else { Vec::new() }
    }

    fn sessions() -> Vec<SessionFacts> {
        vec![session("a", 10, OLD), session("b", 20, OLD), session("c", 30, OLD), session("d", 40, NOW as i64 - 60)]
    }

    const NOT_LIVE: SessionLiveness = SessionLiveness::NotLive;
    const BY_RECORD: SessionLiveness = SessionLiveness::Live { by: LiveBy::Record };
    const BY_RECENT: SessionLiveness = SessionLiveness::Live { by: LiveBy::Recent };
    const BY_OPEN: SessionLiveness = SessionLiveness::Live { by: LiveBy::OpenFile };

    fn unknown(cause: UnknownCause) -> SessionLiveness {
        SessionLiveness::Unknown { cause }
    }

    #[test]
    fn claude_sessions_from_records_times_and_open_files() {
        use UnknownCause as C;
        let all = |cause| vec![unknown(cause); 4];
        type Records = Result<Vec<RecordOutcome>, UnknownCause>;
        let table: Vec<(&str, Records, Vec<SessionLiveness>)> = vec![
            ("no records", Ok(vec![]), vec![NOT_LIVE, NOT_LIVE, BY_OPEN, BY_RECENT]),
            ("a live record", Ok(vec![live("a")]), vec![BY_RECORD, NOT_LIVE, BY_OPEN, BY_RECENT]),
            (
                "a gone process does not block",
                Ok(vec![RecordOutcome::Gone]),
                vec![NOT_LIVE, NOT_LIVE, BY_OPEN, BY_RECENT],
            ),
            (
                "an undecidable record blocks its session only",
                Ok(vec![undecidable("b")]),
                vec![NOT_LIVE, unknown(C::UndecidableSessionRecord), BY_OPEN, BY_RECENT],
            ),
            (
                "a record names the session that is also open",
                Ok(vec![live("c"), undecidable("d")]),
                vec![NOT_LIVE, NOT_LIVE, BY_RECORD, BY_RECENT],
            ),
            ("a missing records folder", Err(C::RecordsMissing), all(C::RecordsMissing)),
            ("an unreadable records folder", Err(C::RecordsUnreadable), all(C::RecordsUnreadable)),
            (
                "one unparsable record beside live and gone ones",
                Ok(vec![live("a"), RecordOutcome::Gone, RecordOutcome::Unparsable]),
                all(C::UnparsableRecord),
            ),
            (
                "an undecidable record without a session",
                Ok(vec![RecordOutcome::Undecidable { session: None }]),
                all(C::UndecidableRecord),
            ),
            (
                "a live record without a session",
                Ok(vec![RecordOutcome::Live { session: None }, live("a")]),
                all(C::LiveRecordWithoutSession),
            ),
            (
                "the widest cause is the one reported",
                Ok(vec![RecordOutcome::Live { session: None }, RecordOutcome::Unparsable]),
                all(C::UnparsableRecord),
            ),
        ];
        for (case, records, want) in table {
            let got = claude(records.as_deref().map_err(|c| *c), &sessions(), &[4141, 4242], open, now());
            assert_eq!(got, want, "{case}");
        }
    }

    #[test]
    fn recent_is_within_ten_minutes_of_any_file_either_side() {
        let at = |modified: Option<i64>| SessionFacts {
            session: "s".into(),
            files: vec![
                FileFact { id: (1, 10), modified: Some(OLD) },
                // A companion: its own time counts.
                FileFact { id: (1, 11), modified },
            ],
        };
        let decide = |s: SessionFacts| claude(Ok(&[]), &[s], &[], |_| Vec::new(), now())[0];
        let window = entries::RECENT.as_secs() as i64;
        assert_eq!(decide(at(Some(NOW as i64 - window))), BY_RECENT);
        assert_eq!(decide(at(Some(NOW as i64 - window - 1))), NOT_LIVE);
        assert_eq!(decide(at(Some(NOW as i64 + 30))), BY_RECENT);
        // A time not known fails closed.
        assert_eq!(decide(at(None)), BY_RECENT);
    }

    #[test]
    fn open_files_are_swept_only_for_undecided_sessions() {
        let asked = std::cell::Cell::new(0);
        let counting = |pid| {
            asked.set(asked.get() + 1);
            open(pid)
        };
        let got = claude(Ok(&[live("c")]), &[session("c", 30, OLD)], &[4242], counting, now());
        assert_eq!((got, asked.get()), (vec![BY_RECORD], 0));
        let got = claude(Err(UnknownCause::RecordsMissing), &sessions(), &[4242], counting, now());
        assert_eq!((got.len(), asked.get()), (4, 0));
        // Another user's process is not in `pids`, so its open file does not count.
        let got = claude(Ok(&[]), &[session("c", 30, OLD)], &[4141], counting, now());
        assert_eq!((got, asked.get()), (vec![NOT_LIVE], 1));
    }

    fn proc(pid: i32, name: &str, exe: &str, args: &[&str]) -> ProcInfo {
        ProcInfo {
            pid,
            name: name.to_owned(),
            exe: Some(PathBuf::from(exe)),
            args: args.iter().map(OsString::from).collect(),
            ..ProcInfo::default()
        }
    }

    fn codex_cli() -> ProcInfo {
        proc(5001, "codex", "/opt/homebrew/bin/codex", &["codex"])
    }

    fn codex_app_server() -> ProcInfo {
        proc(5002, "codex", "/opt/homebrew/bin/codex", &["codex", "app-server"])
    }

    fn chatgpt_codex() -> ProcInfo {
        proc(5003, "codex", "/Applications/ChatGPT.app/Contents/Resources/codex", &["codex", "app-server"])
    }

    fn shell() -> ProcInfo {
        proc(5004, "zsh", "/bin/zsh", &["zsh"])
    }

    /// Fake pid 5004 holds the lock `(2, 77)` open.
    fn lock_open(pid: i32) -> Vec<OpenVnode> {
        if pid == 5004 { vec![OpenVnode { dev: 2, ino: 77, path: None }] } else { Vec::new() }
    }

    #[test]
    fn a_harness_runs_by_its_process_or_a_held_codex_lock() {
        use HarnessCause as C;
        use HarnessId as H;
        const RUNNING: HarnessLiveness = HarnessLiveness::Live { by: HarnessBy::Running };
        const LOCK_HELD: HarnessLiveness = HarnessLiveness::Live { by: HarnessBy::LockHeld };
        const NOT: HarnessLiveness = HarnessLiveness::NotLive;
        let unknown = |cause| HarnessLiveness::Unknown { cause };
        let held = Ok(vec![("t1".to_owned(), (2, 77))]);
        let free = Ok(vec![("t2".to_owned(), (2, 78))]);
        let none = Ok(vec![]);
        let missing = Err(UnknownReason::Missing);
        let unreadable = Err(UnknownReason::Unreadable);
        let cursor_app = proc(6001, "Cursor", "/Applications/Cursor.app/Contents/MacOS/Cursor", &[]);
        let cursor_agent = proc(6002, "cursor-agent", "/Users/example/.local/bin/cursor-agent", &["cursor-agent"]);
        let opencode = proc(6003, "opencode", "/opt/homebrew/bin/opencode", &["opencode"]);
        let table: Vec<(&str, H, Vec<ProcInfo>, &CodexLocks, HarnessLiveness)> = vec![
            ("codex: none running, no lock", H::Codex, vec![shell()], &free, NOT),
            ("codex: no lock folder", H::Codex, vec![shell()], &missing, NOT),
            ("codex: an empty lock folder", H::Codex, vec![shell()], &none, NOT),
            ("codex: a held lock", H::Codex, vec![shell()], &held, LOCK_HELD),
            ("codex: its CLI", H::Codex, vec![codex_cli()], &free, RUNNING),
            ("codex: its app-server", H::Codex, vec![codex_app_server(), shell()], &held, RUNNING),
            ("codex: inside another app", H::Codex, vec![chatgpt_codex()], &free, unknown(C::UnnamedProcess)),
            ("codex: inside another app, a held lock", H::Codex, vec![chatgpt_codex(), shell()], &held, LOCK_HELD),
            ("codex: unreadable locks", H::Codex, vec![shell()], &unreadable, unknown(C::LocksUnreadable)),
            ("codex: unreadable locks, its CLI", H::Codex, vec![codex_cli()], &unreadable, RUNNING),
            ("opencode: running", H::Opencode, vec![opencode.clone()], &unreadable, RUNNING),
            ("opencode: not running", H::Opencode, vec![codex_cli(), shell()], &unreadable, NOT),
            ("cursor: the app", H::Cursor, vec![cursor_app], &none, RUNNING),
            ("cursor: cursor-agent", H::Cursor, vec![cursor_agent], &none, RUNNING),
            ("cursor: not running", H::Cursor, vec![opencode, shell()], &held, NOT),
        ];
        for (case, h, procs, locks, want) in table {
            assert_eq!(harness(h, &procs, Path::new(PACKAGES), locks, lock_open), want, "{case}");
        }
    }

    #[test]
    fn the_codex_sweep_is_skipped_when_a_process_decides() {
        let asked = std::cell::Cell::new(0);
        let counting = |pid| {
            asked.set(asked.get() + 1);
            lock_open(pid)
        };
        let held = Ok(vec![("t1".to_owned(), (2, 77))]);
        let got = harness(HarnessId::Codex, &[codex_cli(), shell()], Path::new(PACKAGES), &held, counting);
        assert_eq!((got, asked.get()), (HarnessLiveness::Live { by: HarnessBy::Running }, 0));
        // Only Codex has locks: opencode never sweeps.
        let got = harness(HarnessId::Opencode, &[shell()], Path::new(PACKAGES), &held, counting);
        assert_eq!((got, asked.get()), (HarnessLiveness::NotLive, 0));
    }
}
