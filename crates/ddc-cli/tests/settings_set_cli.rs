//! `ddc settings set agents_clean_safe on|off` end to end (story "Settings:
//! let coding agents clean safe items", epic DDC-003): the one key changed,
//! the rest and the mode kept, each change journaled, the guard on `on`,
//! and no ddc lock taken.

use ddc_core::platform::Canonical as _;
use ddc_loadgen::isolate::ClearEnv as _;
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime};

use ddc_core::env::Env;
use ddc_core::holders::HolderCommand;
use ddc_core::store::{self, LockMode};

mod common;

struct Fx {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    data: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonical().unwrap();
        let fx = Self { home: root.join("home"), data: root.join("data"), _tmp: tmp };
        fs::create_dir_all(fx.home.join("p")).unwrap();
        fx
    }

    fn settings_path(&self) -> PathBuf {
        self.data.join("settings.json")
    }

    fn journal(&self) -> PathBuf {
        self.data.join("logs/actions.jsonl")
    }

    /// A file with other keys, made private.
    fn full(&self) -> serde_json::Value {
        let json = serde_json::json!({
            "schema_version": 1,
            "roots": [self.home.join("p")],
            "disabled_rules": ["node.node_modules"],
            "min_size": 1024,
            "notify_low_disk": false,
        });
        fs::create_dir_all(&self.data).unwrap();
        fs::write(self.settings_path(), serde_json::to_vec_pretty(&json).unwrap()).unwrap();
        private(&self.settings_path());
        json
    }

    fn file(&self) -> serde_json::Value {
        serde_json::from_slice(&fs::read(self.settings_path()).unwrap()).unwrap()
    }

    fn lines(&self) -> Vec<serde_json::Value> {
        match fs::read_to_string(self.journal()) {
            Ok(text) => text.lines().map(|l| serde_json::from_str(l).unwrap()).collect(),
            Err(_) => Vec::new(),
        }
    }

    fn ddc(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"));
        cmd.clear_env()
            .home_env(&self.home)
            .env("PATH", "")
            .env("NO_COLOR", "1")
            .args(["settings", "set", "agents_clean_safe"])
            .args(args)
            .arg("--data-dir")
            .arg(&self.data)
            .stdin(Stdio::null());
        cmd
    }

    fn set(&self, args: &[&str]) -> Output {
        self.ddc(args).output().unwrap()
    }

    /// `ddc` on a pseudo-terminal answering `answer`: its exit and what it
    /// printed, or `None` where there is no pseudo-terminal driver.
    fn in_terminal(&self, test: &str, args: &[&str], answer: &str) -> Option<(Option<i32>, String)> {
        let ddc = self.ddc(args);
        let mut script =
            common::skipped_without(test, common::script(ddc.get_program(), &ddc.get_args().collect::<Vec<_>>()))?;
        script.clear_env();
        for (k, v) in ddc.get_envs() {
            script.env(k, v.unwrap());
        }
        let mut child = script.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(answer.as_bytes()).unwrap();
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(started.elapsed() < Duration::from_secs(60), "ddc did not finish");
            std::thread::sleep(Duration::from_millis(20));
        };
        drop(stdin);
        let mut out = String::new();
        child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
        Some((status.code(), out))
    }
}

#[cfg(unix)]
fn private(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}
#[cfg(not(unix))]
fn private(_: &Path) {}

#[cfg(unix)]
fn assert_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o600, "the mode is kept");
}
#[cfg(not(unix))]
fn assert_private(_: &Path) {}

fn text(out: &Output) -> (String, String) {
    (String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn on_and_off_change_the_one_key_and_journal_each_change() {
    if common::unsupported("on_and_off_change_the_one_key_and_journal_each_change") {
        return;
    }
    let fx = Fx::new();
    let before = fx.full();

    let out = fx.set(&["on", "--approved"]);

    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(stdout, "agents_clean_safe: on\n");
    let mut want = before.clone();
    want["agents_clean_safe"] = true.into();
    assert_eq!(fx.file(), want, "only the key changed");
    assert_private(&fx.settings_path());
    let lines = fx.lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["kind"], "settings-set");
    assert_eq!(lines[0]["schema_version"], 14);
    assert_eq!(
        (&lines[0]["key"], &lines[0]["value"], &lines[0]["previous"]),
        (&"agents_clean_safe".into(), &true.into(), &false.into())
    );

    let out = fx.set(&["off"]);
    assert_eq!(text(&out).0, "agents_clean_safe: off\n");
    assert_eq!(fx.file(), before, "off removes the key");
    assert_private(&fx.settings_path());
    let lines = fx.lines();
    assert_eq!(lines.len(), 2);
    assert_eq!(
        (&lines[1]["kind"], &lines[1]["value"], &lines[1]["previous"]),
        (&"settings-set".into(), &false.into(), &true.into())
    );

    // Nothing to change: nothing written, nothing journaled.
    let bytes = fs::read(fx.settings_path()).unwrap();
    let out = fx.set(&["off"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out).0, "agents_clean_safe: off (unchanged)\n");
    assert_eq!(fs::read(fx.settings_path()).unwrap(), bytes);
    assert_eq!(fx.lines().len(), 2);
}

#[test]
fn a_fresh_data_folder_gets_no_settings_file_for_nothing() {
    if common::unsupported("a_fresh_data_folder_gets_no_settings_file_for_nothing") {
        return;
    }
    let fx = Fx::new();

    let out = fx.set(&["off"]);

    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    assert!(!fx.settings_path().exists() && !fx.journal().exists());

    let out = fx.set(&["on", "--approved"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    assert_eq!(fx.file(), serde_json::json!({"schema_version": 1, "agents_clean_safe": true}));
}

#[test]
fn on_without_a_terminal_is_refused_and_names_no_way_around_it() {
    if common::unsupported("on_without_a_terminal_is_refused_and_names_no_way_around_it") {
        return;
    }
    let fx = Fx::new();

    let out = fx.set(&["on"]);

    let (_, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("needs you at a terminal, or DeepDriveCleaner's Settings"), "{stderr}");
    assert!(!stderr.contains("--approved"), "{stderr}");
    assert!(!fx.settings_path().exists() && !fx.journal().exists());
    let help = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .clear_env()
        .home_env(&fx.home)
        .args(["settings", "set", "--help"])
        .output()
        .unwrap();
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(help.contains("agents_clean_safe") && !help.contains("--approved"), "{help}");
}

#[test]
fn at_a_terminal_on_says_what_it_allows_and_asks() {
    if common::unsupported("at_a_terminal_on_says_what_it_allows_and_asks") {
        return;
    }
    let fx = Fx::new();

    let Some((code, no)) = fx.in_terminal("at_a_terminal_on_says_what_it_allows_and_asks", &["on"], "n\n") else {
        return;
    };

    // Its own words: the terminal's banner names the disclaimer too.
    let disclaimer = "the disclaimer: https://github.com/SmartAndPoint/DeepDriveCleaner/blob/main/DISCLAIMER.md";
    assert!(no.contains("Coding agents could then remove") && no.contains(disclaimer), "{no}");
    assert!(no.contains("[y/N]") && no.contains("Nothing changed."), "{no}");
    assert_eq!(code, Some(1), "a No exits 1");
    assert!(!fx.settings_path().exists() && !fx.journal().exists(), "a declined answer writes nothing");
    let (code, yes) = fx.in_terminal("at_a_terminal_on_says_what_it_allows_and_asks", &["on"], "y\n").unwrap();
    assert!(yes.contains("agents_clean_safe: on"), "{yes}");
    assert_eq!(code, Some(0));
    assert_eq!(fx.file()["agents_clean_safe"], true);
    assert_eq!(fx.lines().len(), 1);

    // off never asks; nor does --approved, which answers the question.
    let (code, off) = fx.in_terminal("at_a_terminal_on_says_what_it_allows_and_asks", &["off"], "").unwrap();
    assert!(!off.contains("[y/N]") && off.contains("agents_clean_safe: off"), "{off}");
    assert_eq!(code, Some(0));
    let (code, approved) =
        fx.in_terminal("at_a_terminal_on_says_what_it_allows_and_asks", &["on", "--approved"], "").unwrap();
    assert!(!approved.contains("[y/N]") && approved.contains("agents_clean_safe: on"), "{approved}");
    assert_eq!(code, Some(0));
}

#[test]
fn on_when_it_is_on_already_asks_nothing_and_writes_nothing() {
    if common::unsupported("on_when_it_is_on_already_asks_nothing_and_writes_nothing") {
        return;
    }
    let fx = Fx::new();
    assert_eq!(fx.set(&["on", "--approved"]).status.code(), Some(0));
    let bytes = fs::read(fx.settings_path()).unwrap();

    let out = fx.set(&["on"]);

    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    assert_eq!(text(&out).0, "agents_clean_safe: on (unchanged)\n");
    assert_eq!(fs::read(fx.settings_path()).unwrap(), bytes);
    assert_eq!(fx.lines().len(), 1);
}

#[test]
fn a_typo_is_never_answered_with_the_hidden_option() {
    if common::unsupported("a_typo_is_never_answered_with_the_hidden_option") {
        return;
    }
    let fx = Fx::new();
    for typo in ["--approve", "--aproved", "--approval"] {
        let out = fx.set(&["on", typo]);
        let (_, stderr) = text(&out);
        assert_eq!(out.status.code(), Some(2), "{typo}: {stderr}");
        assert!(!stderr.contains("--approved"), "{typo}: {stderr}");
    }
    // A visible option is still suggested.
    let out = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .clear_env()
        .home_env(&fx.home)
        .args(["scan", "--jsn"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&out.stderr).contains("--json"), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!fx.settings_path().exists());
}

#[test]
fn an_unreadable_file_or_journal_changes_nothing() {
    if common::unsupported("an_unreadable_file_or_journal_changes_nothing") {
        return;
    }
    let fx = Fx::new();
    fs::create_dir_all(&fx.data).unwrap();
    fs::write(fx.settings_path(), "{ not json").unwrap();

    let out = fx.set(&["off"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out).1.contains("malformed settings"), "{}", text(&out).1);
    assert_eq!(fs::read_to_string(fx.settings_path()).unwrap(), "{ not json");

    // The journal's folder is a file: the switch is not written.
    let before = fx.full();
    fs::write(fx.data.join("logs"), "not a folder").unwrap();
    let out = fx.set(&["on", "--approved"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out).1.contains("cannot open the action journal"), "{}", text(&out).1);
    assert_eq!(fx.file(), before);
}

#[test]
fn it_takes_no_ddc_lock_so_a_running_clean_never_holds_it_back() {
    if common::unsupported("it_takes_no_ddc_lock_so_a_running_clean_never_holds_it_back") {
        return;
    }
    let fx = Fx::new();
    assert_eq!(fx.set(&["on", "--approved"]).status.code(), Some(0));
    let env = Env::with_paths(&fx.home, &fx.data, &fx.data.join("logs"), SystemTime::now()).unwrap();
    let _clean = store::try_lock(&env, LockMode::Exclusive, HolderCommand::AgentClean).unwrap();

    let out = fx.set(&["off"]);

    assert_eq!(out.status.code(), Some(0), "never 75: {}", text(&out).1);
    assert!(fx.file().get("agents_clean_safe").is_none());
}
