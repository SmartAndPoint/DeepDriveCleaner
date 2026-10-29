//! `ddc disk agents` end to end (story "Coding agents in the Whole disk
//! view"): a temporary HOME and data directory and an empty `PATH`. It
//! reads, takes no lock and writes nothing.

use std::fs;
use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use ddc_core::agents::{AGENTS_SCHEMA_VERSION, AgentsReport, HarnessId, RecordsState, SessionState};
use ddc_loadgen::isolate::ClearEnv as _;

mod contract;

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let fx = Self { root: tmp.path().canonicalize().unwrap(), _tmp: tmp };
        fs::create_dir_all(fx.root.join("data")).unwrap();
        fx
    }

    fn write(&self, rel: &str, content: &str) {
        let path = self.root.join("home").join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn ddc(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"));
        cmd.clear_env()
            .env("HOME", self.root.join("home"))
            .env("PATH", "")
            .env("NO_COLOR", "1")
            .args(["disk", "agents"])
            .args(args)
            .arg("--data-dir")
            .arg(self.root.join("data"))
            .stdin(Stdio::null());
        cmd
    }

    fn json(&self, args: &[&str]) -> AgentsReport {
        let out = self.ddc(&[&["--json"], args].concat()).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        contract::holds("agents", &out.stdout)
    }
}

fn now_ms() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()
}

#[test]
fn the_report_lists_the_harnesses_and_their_live_sessions() {
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

#[test]
fn skips_in_the_settings_bind_the_record_read() {
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

#[test]
fn disk_options_do_not_mix_with_the_subcommand() {
    let fx = Fx::new();
    let out = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .clear_env()
        .env("HOME", fx.root.join("home"))
        .args(["disk", "--json", "agents"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
}
