//! Claude Code's sessions: one `sessions/<pid>.json` per running session,
//! which Claude Code removes when the session ends. Only the named fields
//! of a record are read; `messagingSocketPath` and the `<pid>.<hex>.key`
//! files beside the records never are.
//!
//! Two readers share one listing: [`sessions`], lenient, for `ddc disk
//! agents`, and [`records`], strict, for the liveness check, which reports
//! every record it cannot read instead of passing over it.

use std::io::Read as _;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::platform::procs::{self, ProcStart};
use crate::platform::{PLATFORM, Platform};
use crate::rules::entries::RECENT;

use super::liveness::{RecordOutcome, UnknownCause};
use super::{RecordsState, Session, SessionState, UnknownReason};

/// A record larger than this is not read: Claude Code's are under 1 KiB.
const RECORD_CAP: u64 = 64 * 1024;

/// The `pidDomain` Claude Code writes on this OS, Node's `process.platform`:
/// a record of another domain names a pid of another machine.
const PID_DOMAIN: &str = match PLATFORM {
    Platform::Macos => "darwin",
    Platform::Linux => "linux",
    Platform::Windows => "win32",
};

/// One `<pid>.json` of the records folder.
enum Candidate {
    /// The record, and whether it changed within [`RECENT`] of now.
    Record(Map<String, Value>, bool),
    /// A plain file that could not be read, is larger than [`RECORD_CAP`],
    /// or is not a JSON object.
    Unparsable,
    /// Not a plain file: a FIFO would block the read, and a link is not
    /// followed, so it is never opened.
    NotAFile,
}

/// The records folder as listed.
struct Listing {
    candidates: Vec<Candidate>,
    /// An entry of the listing could not be read: a record may be missing.
    incomplete: bool,
}

/// The `<pid>.json` names of `dir`, each read; `Missing` or `Unreadable`
/// when the folder cannot be listed.
fn list(dir: &Path, now: SystemTime) -> Result<Listing, UnknownReason> {
    let entries = match crate::stats::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(UnknownReason::Missing),
        Err(_) => return Err(UnknownReason::Unreadable),
    };
    let mut listing = Listing { candidates: Vec::new(), incomplete: false };
    for entry in entries {
        let Ok(entry) = entry else {
            listing.incomplete = true;
            continue;
        };
        let name = entry.file_name();
        let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".json")) else { continue };
        if stem.is_empty() || !stem.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let candidate = if !entry.file_type().is_ok_and(|t| t.is_file()) {
            Candidate::NotAFile
        } else {
            let fresh = entry.metadata().and_then(|m| m.modified()).is_ok_and(|m| fresh(m, now));
            read_record(&entry.path()).map_or(Candidate::Unparsable, |r| Candidate::Record(r, fresh))
        };
        listing.candidates.push(candidate);
    }
    Ok(listing)
}

/// The sessions of `dir` (`~/.claude/sessions`), and whether its records
/// could be read at all, at `now`. Lenient: a record that does not parse is
/// passed over, and `Unparsed` is reported only when none parses.
pub(super) fn sessions(dir: &Path, now: SystemTime) -> (RecordsState, Option<UnknownReason>, Vec<Session>) {
    let listing = match list(dir, now) {
        Ok(listing) => listing,
        Err(reason) => return (RecordsState::Unknown, Some(reason), Vec::new()),
    };
    let uid = procs::uid();
    let mut candidates = 0;
    let mut list = Vec::new();
    for candidate in listing.candidates {
        match candidate {
            Candidate::NotAFile => {}
            Candidate::Unparsable => candidates += 1,
            Candidate::Record(record, fresh) => {
                candidates += 1;
                list.push(session(&record, fresh, procs::process_start, uid, PID_DOMAIN));
            }
        }
    }
    if candidates > 0 && list.is_empty() {
        return (RecordsState::Unknown, Some(UnknownReason::Unparsed), Vec::new());
    }
    (RecordsState::Readable, None, list)
}

/// Every record of `dir` (`~/.claude/sessions`) decided: live, gone,
/// undecidable or unparsable, one outcome per `<pid>.json`. Strict: a
/// record that cannot be read, a `<pid>.json` that is not a plain file,
/// and a listing that could not be read whole are reported, never passed
/// over. `start` gives a pid's process start, `uid` is the user's and `now`
/// the time the check runs.
pub(crate) fn records(
    dir: &Path,
    start: impl Fn(i32) -> Option<ProcStart>,
    uid: u32,
    now: SystemTime,
) -> Result<Vec<RecordOutcome>, UnknownCause> {
    let listing = list(dir, now).map_err(|reason| match reason {
        UnknownReason::Missing => UnknownCause::RecordsMissing,
        _ => UnknownCause::RecordsUnreadable,
    })?;
    if listing.incomplete {
        return Err(UnknownCause::RecordsUnreadable);
    }
    Ok(listing
        .candidates
        .into_iter()
        .map(|candidate| match candidate {
            Candidate::Record(record, fresh) => outcome(&record, fresh, &start, uid, PID_DOMAIN),
            Candidate::Unparsable | Candidate::NotAFile => RecordOutcome::Unparsable,
        })
        .collect())
}

/// Changed within [`RECENT`] of `now`, before or after it.
fn fresh(modified: SystemTime, now: SystemTime) -> bool {
    match now.duration_since(modified) {
        Ok(age) => age <= RECENT,
        Err(ahead) => ahead.duration() <= RECENT,
    }
}

/// The record as a JSON map, read up to [`RECORD_CAP`].
fn read_record(path: &Path) -> Option<Map<String, Value>> {
    let mut bytes = Vec::new();
    crate::platform::fsops::open_read(path).ok()?.take(RECORD_CAP + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > RECORD_CAP {
        return None;
    }
    match serde_json::from_slice(&bytes).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

fn string(record: &Map<String, Value>, key: &str) -> Option<String> {
    record.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// One record's outcome for the liveness check. An empty `sessionId`
/// names no session.
fn outcome(
    record: &Map<String, Value>,
    fresh: bool,
    start: impl Fn(i32) -> Option<ProcStart>,
    uid: u32,
    domain: &str,
) -> RecordOutcome {
    let id = || string(record, "sessionId").filter(|s| !s.is_empty());
    match state(record, fresh, start, uid, domain) {
        SessionState::Live => RecordOutcome::Live { session: id() },
        SessionState::Stale => RecordOutcome::Gone,
        SessionState::Unknown => RecordOutcome::Undecidable { session: id() },
    }
}

/// One session from its record; `fresh` says it changed within [`RECENT`],
/// `start` gives a pid's process start, `uid` is the user's and `domain`
/// this OS's `pidDomain`.
pub(super) fn session(
    record: &Map<String, Value>,
    fresh: bool,
    start: impl Fn(i32) -> Option<ProcStart>,
    uid: u32,
    domain: &str,
) -> Session {
    let started_ms = record.get("startedAt").and_then(Value::as_u64);
    Session {
        id: string(record, "sessionId"),
        state: state(record, fresh, start, uid, domain),
        started_at: started_ms
            .map(|ms| humantime::format_rfc3339_seconds(UNIX_EPOCH + Duration::from_millis(ms)).to_string()),
        rollout_time: None,
        status: string(record, "status"),
        entrypoint: string(record, "entrypoint"),
        name: string(record, "name"),
        cwd: string(record, "cwd").map(Into::into),
    }
}

/// Live, stale (its process is gone or another), or unknown: the record
/// lacks a pid or `startedAt`, or names a process of another pid domain
/// than `domain`. On Linux a fresh record that is not live here is unknown
/// too: a container or WSL that shares `~/.claude` writes pids of its own
/// namespace, which here may be no process or another one.
fn state(
    record: &Map<String, Value>,
    fresh: bool,
    start: impl Fn(i32) -> Option<ProcStart>,
    uid: u32,
    domain: &str,
) -> SessionState {
    let pid = record.get("pid").and_then(Value::as_i64).and_then(|p| i32::try_from(p).ok()).filter(|&p| p > 0);
    let started_ms = record.get("startedAt").and_then(Value::as_u64);
    let local = record.get("pidDomain").is_none_or(|d| d.as_str() == Some(domain));
    match (pid, started_ms) {
        (Some(pid), Some(ms)) if local => {
            let proc_start = string(record, "procStart");
            if live(start(pid), uid, ms, proc_start.as_deref()) {
                SessionState::Live
            } else if fresh && domain == "linux" {
                SessionState::Unknown
            } else {
                SessionState::Stale
            }
        }
        _ => SessionState::Unknown,
    }
}

/// Whether the process is `uid`'s and the one that wrote the record: it
/// started no later than `startedAt`, or at the UTC time of `procStart`
/// (a resumed session may keep an older `startedAt`). A reused pid belongs
/// to a process that started later, and fails both.
fn live(proc: Option<ProcStart>, uid: u32, started_ms: u64, proc_start: Option<&str>) -> bool {
    let Some(p) = proc.filter(|p| p.uid == uid) else { return false };
    let start_ms = p.sec.saturating_mul(1000) + p.usec / 1000;
    start_ms <= started_ms || proc_start.is_some_and(|s| s.trim() == utc_ctime(p.sec))
}

/// `sec` as `ctime` prints it in UTC (`%a %b %e %H:%M:%S %Y`), the form of
/// `procStart`.
pub fn utc_ctime(sec: u64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let days = sec / 86_400;
    let rem = sec % 86_400;
    // Howard Hinnant's civil_from_days, for days since 1970-01-01.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{} {} {:>2} {:02}:{:02}:{:02} {}",
        DAYS[(days % 7) as usize],
        MONTHS[(month - 1) as usize],
        day,
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        year
    )
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    /// A fake user and fake pids: never the test's own.
    const UID: u32 = 501;
    const PID: i32 = 4242;

    #[test]
    fn ctime_in_utc() {
        assert_eq!(utc_ctime(0), "Thu Jan  1 00:00:00 1970");
        // 2026-10-28T09:41:07Z, a Wednesday.
        assert_eq!(utc_ctime(1_793_180_467), "Wed Oct 28 09:41:07 2026");
        // 2000-02-29T23:59:59Z.
        assert_eq!(utc_ctime(951_868_799), "Tue Feb 29 23:59:59 2000");
    }

    fn record(v: serde_json::Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    fn mine(sec: u64) -> impl Fn(i32) -> Option<ProcStart> {
        move |_| Some(ProcStart { uid: UID, sec, usec: 0 })
    }

    fn state_of(r: &Map<String, Value>, start: impl Fn(i32) -> Option<ProcStart>) -> SessionState {
        session(r, false, start, UID, "darwin").state
    }

    #[test]
    fn the_start_or_proc_start_proves_a_session_live() {
        let r = record(json!({"pid": PID, "startedAt": 1_000_500, "sessionId": "s"}));
        assert_eq!(state_of(&r, mine(1000)), SessionState::Live);
        // A reused pid: the process started after the record.
        assert_eq!(state_of(&r, mine(2000)), SessionState::Stale);
        assert_eq!(state_of(&r, |_| None), SessionState::Stale);
        // Another user's process.
        let theirs = |_| Some(ProcStart { uid: UID + 1, sec: 1000, usec: 0 });
        assert_eq!(state_of(&r, theirs), SessionState::Stale);
        // Resumed: an older `startedAt`, and `procStart` names this process.
        let resumed = record(json!({"pid": PID, "startedAt": 1_000_500, "procStart": utc_ctime(2000)}));
        assert_eq!(state_of(&resumed, mine(2000)), SessionState::Live);
    }

    #[test]
    fn a_record_without_its_fields_or_from_another_domain_is_unknown() {
        assert_eq!(state_of(&record(json!({"startedAt": 1})), mine(0)), SessionState::Unknown);
        assert_eq!(state_of(&record(json!({"pid": PID})), mine(0)), SessionState::Unknown);
        assert_eq!(state_of(&record(json!({"pid": "7", "startedAt": 1})), mine(0)), SessionState::Unknown);
        let linux = record(json!({"pid": PID, "startedAt": 1_000_500, "pidDomain": "linux"}));
        let darwin = record(json!({"pid": PID, "startedAt": 1_000_500, "pidDomain": "darwin"}));
        // Judged on macOS: a Linux record names another machine's pid.
        assert_eq!(state_of(&linux, mine(1000)), SessionState::Unknown);
        assert_eq!(state_of(&darwin, mine(1000)), SessionState::Live);
        // Judged on Linux: the other way round.
        let on_linux = |r: &Map<String, Value>| session(r, false, mine(1000), UID, "linux").state;
        assert_eq!(on_linux(&linux), SessionState::Live);
        assert_eq!(on_linux(&darwin), SessionState::Unknown);
        assert_eq!(
            PID_DOMAIN,
            match PLATFORM {
                Platform::Macos => "darwin",
                Platform::Linux => "linux",
                Platform::Windows => "win32",
            }
        );
    }

    /// On Linux, a record written within [`RECENT`] that is not live here
    /// may be a container's, sharing `~/.claude`: its pid may be no process
    /// here, or another one (a kernel thread, another user's). Unknown, never
    /// gone. An older one is stale, and macOS keeps today's reading.
    #[test]
    fn a_fresh_linux_record_that_is_not_live_here_is_unknown() {
        let r = record(json!({"pid": PID, "startedAt": 1_000_500, "pidDomain": "linux"}));
        let on_linux = |fresh, start: &dyn Fn(i32) -> Option<ProcStart>| session(&r, fresh, start, UID, "linux").state;
        let none = |_| None;
        let theirs = |_| Some(ProcStart { uid: UID + 1, sec: 1000, usec: 0 });
        let later = mine(2000);
        for start in [&none as &dyn Fn(i32) -> Option<ProcStart>, &theirs, &later] {
            assert_eq!(on_linux(true, start), SessionState::Unknown);
            assert_eq!(on_linux(false, start), SessionState::Stale);
        }
        // A live one is live, fresh or not.
        assert_eq!(on_linux(true, &mine(1000)), SessionState::Live);
        let d = record(json!({"pid": PID, "startedAt": 1_000_500}));
        assert_eq!(session(&d, true, none, UID, "darwin").state, SessionState::Stale);
    }

    /// Fresh is within [`RECENT`] of the check's own time, either side.
    #[test]
    fn fresh_is_within_recent_of_now_either_side() {
        let now = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let minute = Duration::from_secs(60);
        assert!(fresh(now, now) && fresh(now - RECENT, now) && fresh(now + RECENT, now));
        assert!(!fresh(now - RECENT - minute, now) && !fresh(now + RECENT + minute, now));
    }

    #[test]
    fn each_record_has_its_outcome() {
        let o = |v: serde_json::Value| outcome(&record(v), false, mine(1000), UID, "darwin");
        let live = |session: Option<&str>| RecordOutcome::Live { session: session.map(str::to_owned) };
        let undecidable = |session: Option<&str>| RecordOutcome::Undecidable { session: session.map(str::to_owned) };
        assert_eq!(o(json!({"pid": PID, "startedAt": 1_000_500, "sessionId": "s"})), live(Some("s")));
        assert_eq!(o(json!({"pid": PID, "startedAt": 1_000_500})), live(None));
        assert_eq!(o(json!({"pid": PID, "startedAt": 1_000_500, "sessionId": ""})), live(None));
        assert_eq!(o(json!({"pid": PID, "startedAt": 999_000, "sessionId": "s"})), RecordOutcome::Gone);
        assert_eq!(o(json!({"pid": PID, "sessionId": "s"})), undecidable(Some("s")));
        assert_eq!(o(json!({"startedAt": 1})), undecidable(None));
    }

    /// The pid each fake record names, and its fake start.
    fn starts(pid: i32) -> Option<ProcStart> {
        match pid {
            4242 => Some(ProcStart { uid: UID, sec: 1000, usec: 0 }),
            4343 => Some(ProcStart { uid: UID, sec: 5000, usec: 0 }),
            _ => None,
        }
    }

    fn sorted(mut outcomes: Vec<RecordOutcome>) -> Vec<RecordOutcome> {
        outcomes.sort_by_key(|o| format!("{o:?}"));
        outcomes
    }

    #[test]
    fn one_unparsable_record_beside_parsable_ones_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, text: &str| fs::write(dir.path().join(name), text).unwrap();
        write("4242.json", r#"{"pid": 4242, "startedAt": 1000500, "sessionId": "live"}"#);
        write("4343.json", r#"{"pid": 4343, "startedAt": 1000500, "sessionId": "stale"}"#);
        write("4444.json", "{not json");
        // Not a record: the key beside one, another name, and a non-numeric stem.
        write("4242.0a1b.key", "secret");
        write("notes.txt", "x");
        write("x.json", "{not json");

        // An hour after the records were written: none is fresh, so the
        // stale one is stale on Linux too.
        let later = SystemTime::now() + Duration::from_secs(3600);
        let strict = records(dir.path(), starts, UID, later).unwrap();
        assert_eq!(
            sorted(strict),
            sorted(vec![
                RecordOutcome::Live { session: Some("live".into()) },
                RecordOutcome::Gone,
                RecordOutcome::Unparsable
            ])
        );

        // The lenient reader passes over it, as `disk agents` always has.
        let (state, reason, list) = sessions(dir.path(), SystemTime::now());
        assert_eq!((state, reason, list.len()), (RecordsState::Readable, None, 2));
    }

    #[test]
    fn a_record_that_is_not_a_plain_file_or_too_large_is_unparsable() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("4242.json")).unwrap();
        fs::write(dir.path().join("4343.json"), vec![b' '; RECORD_CAP as usize + 1]).unwrap();
        fs::write(dir.path().join("4444.json"), "[1, 2]").unwrap();
        let strict = records(dir.path(), starts, UID, SystemTime::now()).unwrap();
        assert_eq!(strict, vec![RecordOutcome::Unparsable; 3]);
        // Lenient: the folder is not counted, the other two are unparsed.
        let (state, reason, _) = sessions(dir.path(), SystemTime::now());
        assert_eq!((state, reason), (RecordsState::Unknown, Some(UnknownReason::Unparsed)));
    }

    #[test]
    fn a_missing_records_folder_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        assert_eq!(records(&dir.path().join("sessions"), starts, UID, now), Err(UnknownCause::RecordsMissing));
        assert_eq!(records(dir.path(), starts, UID, now), Ok(Vec::new()));
    }
}
