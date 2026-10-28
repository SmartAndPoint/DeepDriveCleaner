//! Claude Code's sessions: one `sessions/<pid>.json` per running session,
//! which Claude Code removes when the session ends. Only the named fields
//! of a record are read; `messagingSocketPath` and the `<pid>.<hex>.key`
//! files beside the records never are.

use std::io::Read as _;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Map, Value};

use crate::sys::procs::{self, ProcStart};

use super::{RecordsState, Session, SessionState, UnknownReason};

/// A record larger than this is not read: Claude Code's are under 1 KiB.
const RECORD_CAP: u64 = 64 * 1024;

/// The sessions of `dir` (`~/.claude/sessions`), and whether its records
/// could be read at all.
pub(super) fn sessions(dir: &Path) -> (RecordsState, Option<UnknownReason>, Vec<Session>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (RecordsState::Unknown, Some(UnknownReason::Missing), Vec::new());
        }
        Err(_) => return (RecordsState::Unknown, Some(UnknownReason::Unreadable), Vec::new()),
    };
    let mut candidates = 0;
    let mut list = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(stem) = name.to_str().and_then(|n| n.strip_suffix(".json")) else { continue };
        // A plain file only: a FIFO would block the read, and a link is not followed.
        if stem.is_empty() || !stem.bytes().all(|b| b.is_ascii_digit()) || !entry.file_type().is_ok_and(|t| t.is_file())
        {
            continue;
        }
        candidates += 1;
        if let Some(record) = read_record(&entry.path()) {
            list.push(session(&record, procs::process_start));
        }
    }
    if candidates > 0 && list.is_empty() {
        return (RecordsState::Unknown, Some(UnknownReason::Unparsed), Vec::new());
    }
    (RecordsState::Readable, None, list)
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

/// One session from its record; `start` gives a pid's process start.
pub(super) fn session(record: &Map<String, Value>, start: impl Fn(i32) -> Option<ProcStart>) -> Session {
    let pid = record.get("pid").and_then(Value::as_i64).and_then(|p| i32::try_from(p).ok()).filter(|&p| p > 0);
    let started_ms = record.get("startedAt").and_then(Value::as_u64);
    let local = record.get("pidDomain").is_none_or(|d| d.as_str() == Some("darwin"));
    let state = match (pid, started_ms) {
        (Some(pid), Some(ms)) if local => {
            let proc_start = string(record, "procStart");
            if live(start(pid), ms, proc_start.as_deref()) { SessionState::Live } else { SessionState::Stale }
        }
        _ => SessionState::Unknown,
    };
    Session {
        id: string(record, "sessionId"),
        state,
        started_at: started_ms
            .map(|ms| humantime::format_rfc3339_seconds(UNIX_EPOCH + Duration::from_millis(ms)).to_string()),
        rollout_time: None,
        status: string(record, "status"),
        entrypoint: string(record, "entrypoint"),
        name: string(record, "name"),
        cwd: string(record, "cwd").map(Into::into),
    }
}

/// Whether the process is this user's and the one that wrote the record:
/// it started no later than `startedAt`, or at the UTC time of `procStart`
/// (a resumed session may keep an older `startedAt`). A reused pid belongs
/// to a process that started later, and fails both.
fn live(proc: Option<ProcStart>, started_ms: u64, proc_start: Option<&str>) -> bool {
    let Some(p) = proc.filter(|p| p.uid == procs::uid()) else { return false };
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
    use serde_json::json;

    use super::*;

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
        move |_| Some(ProcStart { uid: procs::uid(), sec, usec: 0 })
    }

    #[test]
    fn the_start_or_proc_start_proves_a_session_live() {
        let r = record(json!({"pid": 7, "startedAt": 1_000_500, "sessionId": "s"}));
        assert_eq!(session(&r, mine(1000)).state, SessionState::Live);
        // A reused pid: the process started after the record.
        assert_eq!(session(&r, mine(2000)).state, SessionState::Stale);
        assert_eq!(session(&r, |_| None).state, SessionState::Stale);
        // Another user's process.
        let theirs = |_| Some(ProcStart { uid: procs::uid() + 1, sec: 1000, usec: 0 });
        assert_eq!(session(&r, theirs).state, SessionState::Stale);
        // Resumed: an older `startedAt`, and `procStart` names this process.
        let resumed = record(json!({"pid": 7, "startedAt": 1_000_500, "procStart": utc_ctime(2000)}));
        assert_eq!(session(&resumed, mine(2000)).state, SessionState::Live);
    }

    #[test]
    fn a_record_without_its_fields_or_from_another_domain_is_unknown() {
        assert_eq!(session(&record(json!({"startedAt": 1})), mine(0)).state, SessionState::Unknown);
        assert_eq!(session(&record(json!({"pid": 7})), mine(0)).state, SessionState::Unknown);
        assert_eq!(session(&record(json!({"pid": "7", "startedAt": 1})), mine(0)).state, SessionState::Unknown);
        let remote = record(json!({"pid": 7, "startedAt": 1_000_500, "pidDomain": "linux"}));
        assert_eq!(session(&remote, mine(1000)).state, SessionState::Unknown);
        let local = record(json!({"pid": 7, "startedAt": 1_000_500, "pidDomain": "darwin"}));
        assert_eq!(session(&local, mine(1000)).state, SessionState::Live);
    }
}
