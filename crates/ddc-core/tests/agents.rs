//! `ddc disk agents` over a fixture home (story "Coding agents in the Whole
//! disk view"): session records, lock files held by a child process, and
//! sizes of the harness folders.

mod common;

use std::fs::File;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

use common::{TreeBuilder, alloc, reference_alloc};
use ddc_core::agents::{
    self, AgentsOptions, AgentsReport, Harness, HarnessId, RecordsState, SessionState, UnknownReason, utc_ctime,
};
use ddc_core::env::Env;
use ddc_core::platform::{Capability, procs};

const A: &str = "0a1b2c3d-0000-4000-8000-00000000000a";
const B: &str = "0a1b2c3d-0000-4000-8000-00000000000b";
const C: &str = "0a1b2c3d-0000-4000-8000-00000000000c";

struct Fixture {
    t: TreeBuilder,
}

impl Fixture {
    fn new() -> Self {
        let t = TreeBuilder::new();
        t.dir("home");
        t.dir("data");
        Self { t }
    }

    fn home(&self, rel: &str) -> PathBuf {
        self.t.path(&format!("home/{rel}"))
    }

    fn dir(&self, rel: &str) -> PathBuf {
        self.t.dir(&format!("home/{rel}"))
    }

    fn file(&self, rel: &str, len: usize) -> PathBuf {
        self.t.file(&format!("home/{rel}"), len)
    }

    fn write(&self, rel: &str, content: &str) -> PathBuf {
        self.t.write(&format!("home/{rel}"), content)
    }

    fn report_with(&self, sizes: bool, user_skips: Vec<PathBuf>) -> AgentsReport {
        let env =
            Env::with_paths(&self.t.path("home"), &self.t.path("data"), &self.t.path("data/logs"), SystemTime::now())
                .unwrap();
        let opts = AgentsOptions { sizes, threads: Some(2), user_skips };
        agents::report(&env, &opts, &procs::current_user_processes_unopened(), &AtomicBool::new(false))
    }

    fn report(&self) -> AgentsReport {
        self.report_with(true, Vec::new())
    }
}

fn harness(report: &AgentsReport, id: HarnessId) -> &Harness {
    report.harnesses.iter().find(|h| h.id == id).unwrap_or_else(|| panic!("{id:?} missing"))
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

/// A pid no process has: a child that has exited and been reaped.
fn dead_pid() -> u32 {
    let mut child = Command::new("/usr/bin/true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

// MARK: - Claude Code

#[test]
fn claude_sessions_are_live_stale_or_unknown() {
    if common::lacks("claude_sessions_are_live_stale_or_unknown", Capability::CodingAgents) {
        return;
    }
    let fx = Fixture::new();
    let me = std::process::id();
    let start = procs::process_start(me as i32).unwrap();
    let records = [
        // Live: this process, recorded after it started.
        (
            "1.json",
            format!(
                r#"{{"pid":{me},"sessionId":"{A}","startedAt":{},"status":"busy","cwd":"/Users/example/p"}}"#,
                now_ms()
            ),
        ),
        // Stale: no such process.
        ("2.json", format!(r#"{{"pid":{},"sessionId":"{B}","startedAt":{}}}"#, dead_pid(), now_ms())),
        // Stale: a reused pid, recorded before this process started.
        ("3.json", format!(r#"{{"pid":{me},"sessionId":"s3","startedAt":1000}}"#)),
        // Unknown: no pid; it makes no other record stale.
        ("4.json", r#"{"sessionId":"s4","startedAt":1000}"#.to_owned()),
        // Live: resumed, an older `startedAt`, and `procStart` is this process's UTC start.
        (
            "5.json",
            format!(r#"{{"pid":{me},"sessionId":"s5","startedAt":1000,"procStart":"{}"}}"#, utc_ctime(start.sec)),
        ),
        // Live: values Claude Code may add later still decode.
        (
            "6.json",
            format!(
                r#"{{"pid":{me},"sessionId":"s6","startedAt":{},"status":"pondering","entrypoint":"vr-headset","newKey":{{"x":[1]}},"messagingSocketPath":"/tmp/secret.sock"}}"#,
                now_ms()
            ),
        ),
    ];
    for (name, body) in &records {
        fx.write(&format!(".claude/sessions/{name}"), body);
    }
    // Never read: not `<digits>.json`.
    fx.write(".claude/sessions/1.0123abcd.key", "KEYMATERIAL");
    fx.write(".claude/sessions/notes.json", "{");

    let report = fx.report_with(false, Vec::new());
    let s = harness(&report, HarnessId::ClaudeCode).sessions.as_ref().unwrap();
    assert_eq!(s.records, RecordsState::Readable);
    let state = |id: &str| s.list.iter().find(|x| x.id.as_deref() == Some(id)).unwrap().state;
    assert_eq!(state(A), SessionState::Live);
    assert_eq!(state(B), SessionState::Stale);
    assert_eq!(state("s3"), SessionState::Stale);
    assert_eq!(state("s4"), SessionState::Unknown);
    assert_eq!(state("s5"), SessionState::Live);
    assert_eq!(state("s6"), SessionState::Live);
    let s6 = s.list.iter().find(|x| x.id.as_deref() == Some("s6")).unwrap();
    assert_eq!(s6.status.as_deref(), Some("pondering"));
    assert_eq!(s6.entrypoint.as_deref(), Some("vr-headset"));
    // Live first.
    assert!(s.list.iter().take(3).all(|x| x.state == SessionState::Live));

    let json = serde_json::to_string(&report).unwrap();
    for never in ["messagingSocketPath", "secret.sock", "procStart", "KEYMATERIAL", ".key", "newKey"] {
        assert!(!json.contains(never), "the report contains {never}");
    }
}

#[test]
fn records_folder_missing_empty_or_unparsable() {
    let fx = Fixture::new();
    fx.dir(".claude");
    let s = |fx: &Fixture| harness(&fx.report_with(false, Vec::new()), HarnessId::ClaudeCode).sessions.clone().unwrap();
    let missing = s(&fx);
    assert_eq!((missing.records, missing.reason), (RecordsState::Unknown, Some(UnknownReason::Missing)));

    // Claude Code removes records on exit: an empty folder is readable.
    fx.dir(".claude/sessions");
    let empty = s(&fx);
    assert_eq!((empty.records, empty.reason, empty.list.len()), (RecordsState::Readable, None, 0));

    fx.write(".claude/sessions/7.json", "not json");
    fx.write(".claude/sessions/8.json", "[1, 2]");
    let unparsed = s(&fx);
    assert_eq!((unparsed.records, unparsed.reason), (RecordsState::Unknown, Some(UnknownReason::Unparsed)));
    assert!(unparsed.list.is_empty());
}

#[test]
fn transcripts_leave_out_the_live_sessions() {
    if common::unsupported("transcripts_leave_out_the_live_sessions") {
        return;
    }
    let fx = Fixture::new();
    let me = std::process::id();
    // A is live and has no transcript yet; C is live with one; B ended.
    fx.write(".claude/sessions/1.json", &format!(r#"{{"pid":{me},"sessionId":"{A}","startedAt":{}}}"#, now_ms()));
    fx.write(".claude/sessions/2.json", &format!(r#"{{"pid":{me},"sessionId":"{C}","startedAt":{}}}"#, now_ms()));
    fx.file(&format!(".claude/projects/p/{C}.jsonl"), 9000);
    let b = fx.file(&format!(".claude/projects/p/{B}.jsonl"), 20_000);
    let b_sub = fx.file(&format!(".claude/projects/p/{B}/subagents/agent-1.jsonl"), 12_000);
    fx.file(".claude/projects/p/memory/MEMORY.md", 3000);

    let report = fx.report();
    let t = &harness(&report, HarnessId::ClaudeCode).sessions.as_ref().unwrap().transcripts;
    assert_eq!(t.count, Some(1));
    assert_eq!(t.bytes, Some(alloc(&b) + alloc(&b_sub)));
    assert!(!t.skipped);

    // Without sizes the count stays, from the names.
    let fast = fx.report_with(false, Vec::new());
    let t = &harness(&fast, HarnessId::ClaudeCode).sessions.as_ref().unwrap().transcripts;
    assert_eq!((t.count, t.bytes), (Some(1), None));
}

// MARK: - Codex

struct Holder(Child);

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_held_lock_is_an_open_thread() {
    if common::unsupported("a_held_lock_is_an_open_thread") {
        return;
    }
    let fx = Fixture::new();
    let a = fx.write(&format!(".codex/thread-writer-locks/{A}.lock"), "");
    fx.write(&format!(".codex/thread-writer-locks/{B}.lock"), "");
    let coordination = fx.write(".codex/thread-writer-locks/.coordination.lock", "");
    fx.file(&format!(".codex/sessions/2026/10/28/rollout-2026-10-28T09-41-07-{A}.jsonl"), 5000);
    let b = fx.file(&format!(".codex/sessions/2026/10/27/rollout-2026-10-27T08-00-00-{B}.jsonl"), 7000);
    let c = fx.file(&format!(".codex/sessions/2026/10/26/rollout-2026-10-26T08-00-00-{C}.jsonl"), 3000);
    // A child of this test holds A's lock (and the coordination lock) open.
    let _a = Holder(Command::new("/bin/sleep").arg("30").stdin(File::open(&a).unwrap()).spawn().unwrap());
    let _c = Holder(
        Command::new("/bin/sleep")
            .arg("30")
            .stdin(File::open(&coordination).unwrap())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );

    let report = fx.report();
    let codex = harness(&report, HarnessId::Codex);
    let s = codex.sessions.as_ref().unwrap();
    assert_eq!(s.records, RecordsState::Readable);
    let ids: Vec<(&str, SessionState)> = s.list.iter().map(|x| (x.id.as_deref().unwrap(), x.state)).collect();
    assert_eq!(ids, [(A, SessionState::Live), (B, SessionState::Stale)]);
    assert_eq!(s.list[0].rollout_time.as_deref(), Some("2026-10-28T09-41-07"));
    assert_eq!(s.transcripts.count, Some(2));
    assert_eq!(s.transcripts.bytes, Some(alloc(&b) + alloc(&c)));
    // Nested folders: `sessions` exists, `worktrees` does not.
    let nested: Vec<&PathBuf> = codex.folders.iter().filter(|f| f.nested).map(|f| &f.path).collect();
    assert_eq!(nested, [&fx.home(".codex/sessions")]);
}

/// The sweep reads the kernel's `(dev, ino)` of open files and never takes
/// a lock or `stat`s an open path (story "Coding agents in the Whole disk
/// view"): a source test, as `ContractTests` does for the app.
#[test]
fn the_codex_sweep_stats_no_open_path_and_takes_no_lock() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/agents");
    let code = |file: &str| -> String {
        std::fs::read_to_string(root.join(file))
            .unwrap()
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    for file in ["mod.rs", "claude.rs", "codex.rs", "running.rs", "sizes.rs", "model.rs"] {
        let text = code(file);
        for forbidden in
            ["flock", "lsof", "Command::new", "remove_", "trash", "Executor", "plan::", "rusqlite", "OpenOptions"]
        {
            assert!(!text.contains(forbidden), "agents/{file} mentions {forbidden}");
        }
    }
    let codex = code("codex.rs");
    let sweep = &codex[codex.find("fn held(").unwrap()..];
    let sweep = &sweep[..sweep.find("\n}").unwrap()];
    for forbidden in
        ["metadata", "stat", "canonicalize", "exists", "is_file", "is_dir", "File::open", "read_dir", "path"]
    {
        assert!(!sweep.contains(forbidden), "the lock sweep mentions {forbidden}");
    }
}

// MARK: - Sizes, skips and absence

#[test]
fn sizes_count_hard_links_once_and_nested_folders_inclusively() {
    if common::unsupported("sizes_count_hard_links_once_and_nested_folders_inclusively") {
        return;
    }
    let fx = Fixture::new();
    let shared = fx.file(".claude/plugins/big.bin", 256 * 1024);
    fx.t.hardlink(&shared, "home/.claude/projects/p/link.bin");
    fx.file(".claude/projects/p/other.bin", 64 * 1024);
    fx.file("Library/Application Support/Claude/claude-code/2.1.0/claude", 128 * 1024);
    // Chat app state beside it: not Claude Code's.
    fx.file("Library/Application Support/Claude/chat.db", 512 * 1024);

    let report = fx.report();
    let claude = harness(&report, HarnessId::ClaudeCode);
    let folder = |rel: &str| claude.folders.iter().find(|f| f.path == fx.home(rel)).unwrap();
    let top = folder(".claude");
    let projects = folder(".claude/projects");
    let bins = folder("Library/Application Support/Claude/claude-code");
    assert_eq!(top.bytes, Some(reference_alloc(&fx.home(".claude"))));
    assert!(!top.nested && projects.nested);
    assert_eq!(projects.bytes, Some(reference_alloc(&fx.home(".claude/projects"))));
    assert_eq!(claude.bytes, Some(top.bytes.unwrap() + bins.bytes.unwrap()));
    assert_eq!(claude.folders.len(), 3);
}

#[test]
fn an_unreadable_folder_is_unreadable_and_an_unreadable_file_is_still_sized() {
    if common::unsupported("an_unreadable_folder_is_unreadable_and_an_unreadable_file_is_still_sized") {
        return;
    }
    let fx = Fixture::new();
    fx.file(".cursor/extensions/x.bin", 32 * 1024);
    fx.dir(".cursor/locked");
    fx.file(".cursor/locked/inside.bin", 32 * 1024);
    let db = fx.file(".local/share/opencode/opencode.db", 64 * 1024);
    let transcript = fx.file(&format!(".claude/projects/p/{B}.jsonl"), 16 * 1024);
    fx.dir(".claude/sessions");
    for f in [&db, &transcript] {
        common::set_mode(f, 0o000);
    }
    fx.t.lock("home/.cursor/locked");

    let report = fx.report();
    let cursor = harness(&report, HarnessId::Cursor);
    assert!(cursor.folders[0].unreadable >= 1);
    assert!(cursor.folders[0].bytes.unwrap() >= alloc(&fx.home(".cursor/extensions/x.bin")));
    assert!(cursor.sessions.is_none());
    let opencode = harness(&report, HarnessId::Opencode);
    assert_eq!(opencode.bytes, Some(reference_alloc(&fx.home(".local/share/opencode"))));
    assert!(opencode.bytes.unwrap() >= alloc(&db));
    assert!(opencode.sessions.is_none());
    let t = &harness(&report, HarnessId::ClaudeCode).sessions.as_ref().unwrap().transcripts;
    assert_eq!(t.bytes, Some(alloc(&transcript)));
}

#[test]
fn a_skipped_harness_folder_is_never_opened() {
    if common::lacks("a_skipped_harness_folder_is_never_opened", Capability::CodingAgents) {
        return;
    }
    let fx = Fixture::new();
    let me = std::process::id();
    fx.write(".claude/sessions/1.json", &format!(r#"{{"pid":{me},"sessionId":"{A}","startedAt":{}}}"#, now_ms()));
    fx.file(&format!(".claude/projects/p/{B}.jsonl"), 4096);
    fx.file(".codex/config.toml", 100);

    let skip = fx.home(".claude").canonicalize().unwrap();
    let report = fx.report_with(true, vec![skip]);
    let claude = harness(&report, HarnessId::ClaudeCode);
    assert!(claude.folders.iter().all(|f| f.skipped && f.bytes.is_none()));
    assert_eq!(claude.bytes, None, "nothing measured is not 0 bytes");
    let s = claude.sessions.as_ref().unwrap();
    assert_eq!((s.records, s.reason), (RecordsState::Unknown, Some(UnknownReason::Skipped)));
    assert!(s.list.is_empty());
    assert!(s.transcripts.skipped && s.transcripts.count.is_none() && s.transcripts.bytes.is_none());
    assert!(harness(&report, HarnessId::Codex).folders[0].bytes.is_some());

    // A skip inside: the sessions stay, the transcripts do not.
    let skip = fx.home(".claude/projects").canonicalize().unwrap();
    let report = fx.report_with(true, vec![skip]);
    let claude = harness(&report, HarnessId::ClaudeCode);
    let projects = claude.folders.iter().find(|f| f.nested).unwrap();
    assert!(projects.skipped && projects.bytes.is_none());
    let s = claude.sessions.as_ref().unwrap();
    assert_eq!(s.records, RecordsState::Readable);
    assert_eq!(s.list[0].state, SessionState::Live);
    assert!(s.transcripts.skipped && s.transcripts.count.is_none());

    // A skip one level further down: the listing stops there too.
    fx.file(&format!(".claude/projects/q/{A}.jsonl"), 4096);
    fx.file(&format!(".codex/sessions/2026/10/28/rollout-2026-10-28T09-41-07-{B}.jsonl"), 4096);
    let unskipped = fx.report_with(true, Vec::new());
    let count = |r: &AgentsReport, id| harness(r, id).sessions.as_ref().unwrap().transcripts.clone();
    assert_eq!(count(&unskipped, HarnessId::Codex).count, Some(1));
    let skips = vec![
        fx.home(".claude/projects/q").canonicalize().unwrap(),
        fx.home(".codex/sessions/2026").canonicalize().unwrap(),
    ];
    let report = fx.report_with(true, skips);
    for id in [HarnessId::ClaudeCode, HarnessId::Codex] {
        let t = count(&report, id);
        assert!(t.skipped && t.count.is_none() && t.bytes.is_none(), "{id:?}: {t:?}");
    }
}

#[test]
fn a_harness_without_folders_is_absent_and_no_sizes_leaves_bytes_out() {
    let fx = Fixture::new();
    assert!(fx.report().harnesses.is_empty());

    fx.file(".cache/opencode/x", 4096);
    let report = fx.report_with(false, Vec::new());
    assert!(!report.sizes);
    assert_eq!(report.harnesses.iter().map(|h| h.id).collect::<Vec<_>>(), [HarnessId::Opencode]);
    let opencode = &report.harnesses[0];
    assert!(opencode.bytes.is_none() && opencode.folders.iter().all(|f| f.bytes.is_none()));
    assert!(opencode.sessions.is_none());
}

#[test]
#[allow(clippy::disallowed_methods)] // DDC_UPDATE_SCHEMA regenerates the committed file
fn committed_agents_schema_is_current() {
    let schema = serde_json::to_string_pretty(&schemars::schema_for!(AgentsReport)).unwrap() + "\n";
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/agents.schema.json");
    if std::env::var_os("DDC_UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &schema).unwrap();
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(committed == schema, "docs/agents.schema.json is stale: run DDC_UPDATE_SCHEMA=1 cargo test");
}
