//! `ddc disk agents` end to end (story "Coding agents in the Whole disk
//! view"): a temporary HOME and data directory and an empty `PATH`. It
//! reads, takes no lock and writes nothing.

use ddc_core::platform::Canonical as _;
use std::fs;
use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use ddc_core::agents::{AGENTS_SCHEMA_VERSION, AgentsReport, FootprintState, HarnessId, RecordsState, SessionState};
use ddc_core::platform::{Capability, PLATFORM, Platform, meta, procs};
use ddc_loadgen::isolate::ClearEnv as _;

mod common;
mod contract;

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let fx = Self { root: tmp.path().canonical().unwrap(), _tmp: tmp };
        fs::create_dir_all(fx.root.join("data")).unwrap();
        fx
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.root.join("home").join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    /// Claude Code's temporary folder under the fixture's system root, from the root.
    fn temp_rel() -> String {
        let tmp = if PLATFORM == Platform::Macos { "private/tmp" } else { "tmp" };
        format!("sys/{tmp}/claude-{}", procs::uid())
    }

    /// A file of `len` bytes at `rel`, from the root.
    fn file(&self, rel: &str, len: usize) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![b'x'; len]).unwrap();
    }

    /// Always under a fixture system root: no test reads the real `/tmp`.
    fn ddc(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"));
        cmd.clear_env()
            .env_remove("CLAUDE_CODE_TMPDIR")
            .env_remove("TMPDIR")
            .home_env(self.root.join("home"))
            .env("PATH", "")
            .env("NO_COLOR", "1")
            .args(["disk", "agents"])
            .args(args)
            .arg("--data-dir")
            .arg(self.root.join("data"))
            .arg("--system-root")
            .arg(self.root.join("sys"))
            .stdin(Stdio::null());
        cmd
    }

    fn json(&self, args: &[&str]) -> AgentsReport {
        Self::json_of(self.ddc(&[&["--json"], args].concat()))
    }

    fn json_of(mut cmd: Command) -> AgentsReport {
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        contract::holds("agents", &out.stdout)
    }
}

fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()
}

#[test]
fn the_report_lists_the_harnesses_and_their_live_sessions() {
    if common::unsupported("the_report_lists_the_harnesses_and_their_live_sessions")
        || common::lacks("the_report_lists_the_harnesses_and_their_live_sessions", Capability::CodingAgents)
    {
        return;
    }
    let fx = Fx::new();
    // This test process is the session's process.
    let me = std::process::id();
    fx.write(
        ".claude/sessions/1.json",
        &format!(
            r#"{{"pid":{me},"sessionId":"s1","startedAt":{},"status":"idle","cwd":"/Users/example/p"}}"#,
            now_ms()
        ),
    );
    fx.write(".claude/projects/p/0a1b2c3d-0000-4000-8000-00000000000b.jsonl", "{}\n");
    fx.write(".cursor/argv.json", "{}");

    let r = fx.json(&[]);
    assert_eq!(r.schema_version, AGENTS_SCHEMA_VERSION);
    assert!(r.sizes);
    assert_eq!(r.harnesses.iter().map(|h| h.id).collect::<Vec<_>>(), [HarnessId::ClaudeCode, HarnessId::Cursor]);
    let claude = &r.harnesses[0];
    assert!(claude.bytes.is_some_and(|b| b > 0));
    let s = claude.sessions.as_ref().unwrap();
    assert_eq!(s.records, RecordsState::Readable);
    assert_eq!(s.list[0].state, SessionState::Live);
    assert_eq!(s.transcripts.count, Some(1));

    // `--no-sizes`: the sessions, without a walk.
    let fast = fx.json(&["--no-sizes"]);
    assert!(!fast.sizes && fast.harnesses.iter().all(|h| h.bytes.is_none()));
    assert_eq!(fast.harnesses[0].sessions.as_ref().unwrap().list[0].state, SessionState::Live);

    // The table names the session's folder and writes nothing.
    let out = fx.ddc(&[]).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(out.status.success());
    assert!(text.contains("Claude Code") && text.contains("~/.claude") && text.contains("live"), "{text}");
    assert_eq!(fs::read_dir(fx.root.join("data")).unwrap().count(), 0, "it saved something");
}

const LIVE: &str = "0a1b2c3d-0000-4000-8000-00000000000a";
const ENDED: &str = "0a1b2c3d-0000-4000-8000-00000000000b";

/// Story "Per-session footprint: what each agent session keeps in its
/// transcripts, temporary folders and worktrees": each session's bytes by
/// kind, valid against the schema, with the temporary folder under the
/// system root even when `CLAUDE_CODE_TMPDIR` names another.
#[test]
fn each_sessions_footprint_and_the_temporary_folder() {
    if common::unsupported("each_sessions_footprint_and_the_temporary_folder")
        || common::lacks("each_sessions_footprint_and_the_temporary_folder", Capability::CodingAgents)
    {
        return;
    }
    let fx = Fx::new();
    let me = std::process::id();
    fx.write(".claude/sessions/1.json", &format!(r#"{{"pid":{me},"sessionId":"{LIVE}","startedAt":{}}}"#, now_ms()));
    fx.write(&format!(".claude/projects/-p/{LIVE}.jsonl"), "{}\n");
    fx.write(&format!(".claude/file-history/{ENDED}/f"), "x");
    // Ended an hour ago: no record, and its transcript older than a recent write.
    let ended = fx.root.join(format!("home/.claude/projects/-p/{ENDED}.jsonl"));
    fx.write(&format!(".claude/projects/-p/{ENDED}.jsonl"), "{}\n");
    let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    fs::File::options().write(true).open(&ended).unwrap().set_modified(hour_ago).unwrap();
    let temp = Fx::temp_rel();
    fx.file(&format!("{temp}/-p/{LIVE}/scratchpad/x"), 64 * 1024);
    fx.file(&format!("{temp}/-p/{ENDED}/tasks/y"), 8 * 1024);
    // No session's: the remainder.
    fx.file(&format!("{temp}/bash-edit-diff/z"), 5000);
    let stray = meta::lstat(&fx.root.join(&temp).join("bash-edit-diff/z")).unwrap().alloc;
    // The variable names another folder: the fixture root wins over it.
    let decoy = "0a1b2c3d-0000-4000-8000-0000000000dd";
    fx.file(&format!("decoy/claude-{}/-p/{decoy}/x", procs::uid()), 4096);

    let mut cmd = fx.ddc(&["--json"]);
    cmd.env("CLAUDE_CODE_TMPDIR", fx.root.join("decoy"));
    let r = Fx::json_of(cmd);
    let s = r.harnesses[0].sessions.as_ref().unwrap();
    let list = s.footprints.as_ref().unwrap();
    let rows: Vec<(&str, FootprintState)> = list.iter().map(|f| (f.id.as_str(), f.state)).collect();
    assert_eq!(rows, [(LIVE, FootprintState::Live), (ENDED, FootprintState::NotLive)]);
    assert!(list.iter().all(|f| f.temp > 0 && f.total == f.transcript + f.checkpoints + f.temp));
    assert!(list[1].transcript > 0 && list[1].checkpoints > 0);
    let root = s.temp_root.as_ref().unwrap();
    assert_eq!(root.path, fx.root.join(&temp));
    let sessions: u64 = list.iter().map(|f| f.temp).sum();
    assert_eq!(root.bytes.unwrap() - sessions, stray, "the remainder is the folder no session owns");

    // `--no-sizes`: neither.
    let fast = fx.json(&["--no-sizes"]);
    let s = fast.harnesses[0].sessions.as_ref().unwrap();
    assert!(s.footprints.is_none() && s.temp_root.is_none());
}

#[test]
fn the_table_shows_ten_sessions_and_the_rest_in_one_line() {
    if common::unsupported("the_table_shows_ten_sessions_and_the_rest_in_one_line")
        || common::lacks("the_table_shows_ten_sessions_and_the_rest_in_one_line", Capability::CodingAgents)
    {
        return;
    }
    let fx = Fx::new();
    fs::create_dir_all(fx.root.join("home/.claude/sessions")).unwrap();
    for i in 0..11 {
        let id = format!("0a1b2c3d-0000-4000-8000-0000000000{i:02}");
        fx.file(&format!("home/.claude/projects/-p/{id}.jsonl"), (i + 1) * 16 * 1024);
        fx.file(&format!("{}/-p/{id}/scratchpad/x", Fx::temp_rel()), 4096);
    }
    let out = fx.ddc(&[]).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(out.status.success());
    assert!(text.contains("sessions, largest first") && text.contains("temporary folder"), "{text}");
    assert_eq!(text.matches("0a1b2c3d ").count(), 10, "{text}");
    assert!(text.contains("1 more, "), "{text}");
    for word in ["delete", "trash", "remove"] {
        assert!(!text.to_lowercase().contains(word), "{text}");
    }
}

#[test]
fn the_table_says_why_the_temporary_folder_was_not_read() {
    if common::unsupported("the_table_says_why_the_temporary_folder_was_not_read")
        || common::lacks("the_table_says_why_the_temporary_folder_was_not_read", Capability::CodingAgents)
    {
        return;
    }
    let id = "0a1b2c3d-0000-4000-8000-000000000001";
    let text_of = |fx: &Fx| {
        let out = fx.ddc(&[]).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8(out.stdout).unwrap()
    };

    #[cfg(unix)]
    {
        let fx = Fx::new();
        fx.file(&format!("home/.claude/projects/-p/{id}.jsonl"), 4096);
        fx.file("elsewhere/-p/x", 4096);
        let base = fx.root.join(Fx::temp_rel());
        fs::create_dir_all(base.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(fx.root.join("elsewhere"), &base).unwrap();
        let text = text_of(&fx);
        assert!(text.contains("not read: a symbolic link"), "{text}");
    }

    let fx = Fx::new();
    fx.file(&format!("home/.claude/projects/-p/{id}.jsonl"), 4096);
    fx.file(&Fx::temp_rel(), 16);
    let text = text_of(&fx);
    assert!(text.contains("not read: not a folder"), "{text}");
}

#[test]
fn skips_in_the_settings_bind_the_record_read() {
    if common::unsupported("skips_in_the_settings_bind_the_record_read")
        || common::lacks("skips_in_the_settings_bind_the_record_read", Capability::CodingAgents)
    {
        return;
    }
    let fx = Fx::new();
    fx.write(".claude/sessions/1.json", r#"{"pid":1,"sessionId":"s1","startedAt":1}"#);
    let skip = fx.root.join("home/.claude");
    fs::write(fx.root.join("data/settings.json"), format!(r#"{{"schema_version":1,"skips":["{}"]}}"#, skip.display()))
        .unwrap();
    let r = fx.json(&[]);
    let s = r.harnesses[0].sessions.as_ref().unwrap();
    assert_eq!(s.records, RecordsState::Unknown);
    assert!(s.list.is_empty() && s.transcripts.skipped);
}

#[test]
fn no_harness_is_an_empty_list_and_a_closed_pipe_is_success() {
    if common::unsupported("no_harness_is_an_empty_list_and_a_closed_pipe_is_success")
        || common::lacks("no_harness_is_an_empty_list_and_a_closed_pipe_is_success", Capability::CodingAgents)
    {
        return;
    }
    let fx = Fx::new();
    fs::create_dir_all(fx.root.join("home")).unwrap();
    assert!(fx.json(&[]).harnesses.is_empty());

    let mut child = fx.ddc(&["--json"]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    drop(child.stdout.take());
    let status = child.wait().unwrap();
    let mut err = String::new();
    child.stderr.take().unwrap().read_to_string(&mut err).unwrap();
    assert!(status.success(), "{status:?}: {err}");
}

/// The app's agents fixtures, written by hand for DDCKit's tests: each is
/// valid against the schema and reads back as an `AgentsReport` that writes
/// the same JSON, so it holds no field the CLI does not write. Except one:
/// `future_field`, the key DDCKit's test proves the app reads past, taken
/// out first, and only where it was put.
#[test]
fn the_apps_agents_fixtures_are_reports_the_cli_could_write() {
    fn without_future_fields(v: &mut serde_json::Value) -> usize {
        match v {
            serde_json::Value::Object(map) => {
                let here = usize::from(map.remove("future_field").is_some());
                here + map.values_mut().map(without_future_fields).sum::<usize>()
            }
            serde_json::Value::Array(items) => items.iter_mut().map(without_future_fields).sum(),
            _ => 0,
        }
    }
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../app/Packages/DDCKit/Tests/DDCKitTests/Fixtures");
    for (name, future) in [("agents-report.json", 1), ("agents-fast.json", 1)] {
        let mut json: serde_json::Value = serde_json::from_slice(&fs::read(dir.join(name)).unwrap()).unwrap();
        assert_eq!(without_future_fields(&mut json), future, "{name}: the unknown keys the app reads past");
        let r: AgentsReport = contract::holds("agents", &serde_json::to_vec(&json).unwrap());
        assert_eq!(r.schema_version, AGENTS_SCHEMA_VERSION, "{name}");
    }
}

#[test]
fn disk_options_do_not_mix_with_the_subcommand() {
    let fx = Fx::new();
    let out = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .clear_env()
        .home_env(fx.root.join("home"))
        .args(["disk", "--json", "agents"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}
