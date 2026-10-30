//! The liveness facts through the process table's seam (ADR "Agent
//! footprint: worktrees, harness data and bounded consent for agents", §3):
//! Claude Code's records read from a fake home, process starts and open
//! files injected for fake pids, never the test's own.

mod common;

use std::time::{Duration, SystemTime};

use common::fakes::FakeProcs;
use common::{TreeBuilder, id};
use ddc_core::agents::liveness::{self, FileFact, LiveBy, SessionFacts, SessionLiveness, UnknownCause};
use ddc_core::exec::{OpenVnode, ProcStart, ProcessTable};
use ddc_core::platform::procs;

const A: &str = "0a1b2c3d-0000-4000-8000-00000000000a";
const B: &str = "0a1b2c3d-0000-4000-8000-00000000000b";
const C: &str = "0a1b2c3d-0000-4000-8000-00000000000c";

/// A session's transcript: its identity, and a change an hour before `now`.
fn facts(t: &TreeBuilder, session: &str, now: SystemTime) -> SessionFacts {
    let path = t.path(&format!("home/.claude/projects/p/{session}.jsonl"));
    let hour_ago = now.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as i64 - 3600;
    SessionFacts { session: session.to_owned(), files: vec![FileFact { id: id(&path), modified: Some(hour_ago) }] }
}

#[test]
fn records_starts_and_open_files_decide_each_session() {
    if common::unsupported("records_starts_and_open_files_decide_each_session") {
        return;
    }
    let t = TreeBuilder::new();
    let records = t.dir("home/.claude/sessions");
    // 4242 runs and wrote its record after it started; 4343 is gone.
    t.write(
        "home/.claude/sessions/4242.json",
        &format!(r#"{{"pid": 4242, "startedAt": 1000500, "sessionId": "{A}"}}"#),
    );
    t.write(
        "home/.claude/sessions/4343.json",
        &format!(r#"{{"pid": 4343, "startedAt": 1000500, "sessionId": "{B}"}}"#),
    );
    for s in [A, B, C] {
        t.write(&format!("home/.claude/projects/p/{s}.jsonl"), "{}\n");
    }
    let c = id(&t.path(&format!("home/.claude/projects/p/{C}.jsonl")));
    let table = FakeProcs::new(Vec::new())
        .with_start(4242, ProcStart { uid: procs::uid(), sec: 1000, usec: 0 })
        .with_vnodes(4444, vec![OpenVnode { dev: c.0, ino: c.1, path: None }]);
    let now = SystemTime::now() + Duration::from_secs(3600);
    let sessions: Vec<SessionFacts> = [A, B, C].iter().map(|s| facts(&t, s, now)).collect();
    let decide = |outcomes: &[liveness::RecordOutcome]| {
        liveness::claude(Ok(outcomes), &sessions, &[4242, 4444], |pid| table.open_vnodes(pid), now)
    };

    let read = liveness::claude_records(&records, &table).unwrap();
    assert_eq!(
        decide(&read),
        [
            SessionLiveness::Live { by: LiveBy::Record },
            SessionLiveness::NotLive,
            SessionLiveness::Live { by: LiveBy::OpenFile }
        ]
    );

    // One record that does not parse leaves every session unknown.
    t.write("home/.claude/sessions/4545.json", "{\"pid\": 45");
    let read = liveness::claude_records(&records, &table).unwrap();
    assert_eq!(decide(&read), [SessionLiveness::Unknown { cause: UnknownCause::UnparsableRecord }; 3]);

    // No records folder at all: an older Claude Code, every session unknown.
    let missing = liveness::claude_records(&t.path("home/.claude/none"), &table);
    assert_eq!(missing, Err(UnknownCause::RecordsMissing));
}
