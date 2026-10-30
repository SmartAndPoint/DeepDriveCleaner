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
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::platform::procs::{self, ProcStart};

use super::liveness::{RecordOutcome, UnknownCause};
use super::{RecordsState, Session, SessionState, UnknownReason};

/// A record larger than this is not read: Claude Code's are under 1 KiB.
const RECORD_CAP: u64 = 64 * 1024;

/// One `<pid>.json` of the records folder.
enum Candidate {
    Record(Map<String, Value>),
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
fn list(dir: &Path) -> Result<Listing, UnknownReason> {
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
            read_record(&entry.path()).map_or(Candidate::Unparsable, Candidate::Record)
        };
        listing.candidates.push(candidate);
    }
    Ok(listing)
}

/// The sessions of `dir` (`~/.claude/sessions`), and whether its records
/// could be read at all. Lenient: a record that does not parse is passed
/// over, and `Unparsed` is reported only when none parses.
pub(super) fn sessions(dir: &Path) -> (RecordsState, Option<UnknownReason>, Vec<Session>) {
    let listing = match list(dir) {
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
            Candidate::Record(record) => {
                candidates += 1;
                list.push(session(&record, procs::process_start, uid));
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
/// over. `start` gives a pid's process start and `uid` is the user's.
pub(crate) fn records(
    dir: &Path,
    start: impl Fn(i32) -> Option<ProcStart>,
    uid: u32,
) -> Result<Vec<RecordOutcome>, UnknownCause> {
    let listing = list(dir).map_err(|reason| match reason {
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
            Candidate::Record(record) => outcome(&record, &start, uid),
            Candidate::Unparsable | Candidate::NotAFile => RecordOutcome::Unparsable,
        })
        .collect())
}

/// The record as a JSON map, read up to [`RECORD_CAP`].
fn read_record(path: &Path) -> Option<Map<String, Value>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path).ok()?.take(RECORD_CAP + 1).read_to_end(&mut bytes).ok()?;
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
fn outcome(record: &Map<String, Value>, start: impl Fn(i32) -> Option<ProcStart>, uid: u32) -> RecordOutcome {
    let id = || string(record, "sessionId").filter(|s| !s.is_empty());
    match state(record, start, uid) {
        SessionState::Live => RecordOutcome::Live { session: id() },
        SessionState::Stale => RecordOutcome::Gone,
        SessionState::Unknown => RecordOutcome::Undecidable { session: id() },
    }
}

/// One session from its record; `start` gives a pid's process start and
/// `uid` is the user's.
pub(super) fn session(record: &Map<String, Value>, start: impl Fn(i32) -> Option<ProcStart>, uid: u32) -> Session {
    let started_ms = record.get("startedAt").and_then(Value::as_u64);
    Session {
        id: string(record, "sessionId"),
        state: state(record, start, uid),
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
/// lacks a pid or `startedAt`, or names a process of another pid domain.
fn state(record: &Map<String, Value>, start: impl Fn(i32) -> Option<ProcStart>, uid: u32) -> SessionState {
    let pid = record.get("pid").and_then(Value::as_i64).and_then(|p| i32::try_from(p).ok()).filter(|&p| p > 0);
    let started_ms = record.get("startedAt").and_then(Value::as_u64);
    let local = record.get("pidDomain").is_none_or(|d| d.as_str() == Some("darwin"));
    match (pid, started_ms) {
        (Some(pid), Some(ms)) if local => {
            let proc_start = string(record, "procStart");
            if live(start(pid), uid, ms, proc_start.as_deref()) { SessionState::Live } else { SessionState::Stale }
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
        session(r, start, UID).state
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
        let remote = record(json!({"pid": PID, "startedAt": 1_000_500, "pidDomain": "linux"}));
        assert_eq!(state_of(&remote, mine(1000)), SessionState::Unknown);
        let local = record(json!({"pid": PID, "startedAt": 1_000_500, "pidDomain": "darwin"}));
        assert_eq!(state_of(&local, mine(1000)), SessionState::Live);
    }

    #[test]
    fn each_record_has_its_outcome() {
        let o = |v: serde_json::Value| outcome(&record(v), mine(1000), UID);
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

        let strict = records(dir.path(), starts, UID).unwrap();
        assert_eq!(
            sorted(strict),
            sorted(vec![
                RecordOutcome::Live { session: Some("live".into()) },
                RecordOutcome::Gone,
                RecordOutcome::Unparsable
            ])
        );

        // The lenient reader passes over it, as `disk agents` always has.
        let (state, reason, list) = sessions(dir.path());
        assert_eq!((state, reason, list.len()), (RecordsState::Readable, None, 2));
    }

    #[test]
    fn a_record_that_is_not_a_plain_file_or_too_large_is_unparsable() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("4242.json")).unwrap();
        fs::write(dir.path().join("4343.json"), vec![b' '; RECORD_CAP as usize + 1]).unwrap();
        fs::write(dir.path().join("4444.json"), "[1, 2]").unwrap();
        let strict = records(dir.path(), starts, UID).unwrap();
        assert_eq!(strict, vec![RecordOutcome::Unparsable; 3]);
        // Lenient: the folder is not counted, the other two are unparsed.
        let (state, reason, _) = sessions(dir.path());
        assert_eq!((state, reason), (RecordsState::Unknown, Some(UnknownReason::Unparsed)));
    }

    #[test]
    fn a_missing_records_folder_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(records(&dir.path().join("sessions"), starts, UID), Err(UnknownCause::RecordsMissing));
        assert_eq!(records(dir.path(), starts, UID), Ok(Vec::new()));
    }
}
