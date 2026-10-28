//! `ddc disk free` end to end (story "Whole disk: free space with the owning
//! tool"): a temporary HOME and data directory, the system's folders and
//! tools under `--system-root`, and an empty `PATH`. `simctl`, `xcodebuild`,
//! `xcode-select` and `brew` are shell scripts that log their calls and
//! touch only the fixture; nothing real is run.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::SystemTime;

use ddc_core::env::Env;
use ddc_core::free::{ActionResult, FreeReport, Refusal, ReportEdit};
use ddc_core::store::{self, LockMode};

const SIMCTL: &str = "Library/Developer/PrivateFrameworks/CoreSimulator.framework/Versions/A/Resources/bin/simctl";
const ASSET: &str = "sys/System/Library/AssetsV2/com_apple_MobileAsset_iOSSimulatorRuntime/a.asset";

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let fx = Self { root: tmp.path().canonicalize().unwrap(), _tmp: tmp };
        let r = fx.root.display().to_string();
        fx.write(&format!("{ASSET}/AssetData/runtime.dmg"), 300_000);
        fx.write("sys/System/Volumes/Data/opt/homebrew/Cellar/wget/1.0/wget", 40_000);
        fx.write("sys/System/Volumes/Data/Library/Developer/CoreSimulator/Caches/dyld", 10_000);
        fx.write("home/Library/Caches/Homebrew/downloads/old.tar.gz", 50_000);
        std::os::unix::fs::symlink("System/Volumes/Data/opt", fx.root.join("sys/opt")).unwrap();
        fx.tool("sys/usr/sbin/diskutil", "exit 1");
        fx.tool("sys/usr/bin/xcode-select", &format!("echo {r}/sys/Dev"));
        fx.tool("sys/Dev/usr/bin/xcodebuild", "exit 0");
        let data = format!("{r}/{ASSET}/AssetData");
        let runtimes = format!(
            r#"{{"A": {{"identifier": "A", "runtimeIdentifier": "com.apple.CoreSimulator.SimRuntime.iOS-27-1", "version": "27.1", "sizeBytes": 300000, "deletable": true, "state": "Ready", "path": "{data}", "parentMountPath": "{data}"}}}}"#
        );
        fx.tool(
            &format!("sys/{SIMCTL}"),
            &format!(
                r#"echo "$* | $DEVELOPER_DIR" >> {r}/simctl-calls
case "$*" in
  "runtime list -j") if [ -f {r}/deleted ]; then echo '{{}}'; else echo '{runtimes}'; fi ;;
  "list devices -j") echo '{{"devices": {{}}}}' ;;
  "runtime delete A --dry-run") echo "Would delete runtime A" ;;
  "runtime delete A") : > {r}/deleted; /bin/rm -rf {r}/{ASSET} ;;
  *) exit 1 ;;
esac"#
            ),
        );
        fx.tool(
            "sys/System/Volumes/Data/opt/homebrew/bin/brew",
            &format!(
                r#"echo "$* | $HOMEBREW_NO_AUTOREMOVE $HOMEBREW_NO_ENV_HINTS $NO_COLOR" >> {r}/brew-calls
case "$*" in
  --prefix) echo {r}/sys/opt/homebrew ;;
  --cellar) echo {r}/sys/opt/homebrew/Cellar ;;
  --cache) echo {r}/home/Library/Caches/Homebrew ;;
  "cleanup -n") echo "Would remove: {r}/sys/opt/homebrew/Cellar/wget/1.0 (1 file, 39KB)"
    echo "==> This operation would free approximately 39KB of disk space." ;;
  cleanup) if [ -f {r}/brew-denied ]; then echo "Error: Permission denied @ apply2files" >&2; exit 1; fi
    /bin/rm -rf {r}/sys/opt/homebrew/Cellar/wget/1.0 {r}/home/Library/Caches/Homebrew/downloads ;;
  *) exit 1 ;;
esac"#
            ),
        );
        fx
    }

    fn write(&self, rel: &str, len: usize) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![7u8; len]).unwrap();
    }

    fn tool(&self, rel: &str, body: &str) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn ddc(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"));
        cmd.env_clear()
            .env("HOME", self.root.join("home"))
            .env("PATH", "")
            .env("NO_COLOR", "1")
            .arg("disk")
            .args(args)
            .arg("--data-dir")
            .arg(self.root.join("data"))
            .arg("--system-root")
            .arg(self.root.join("sys"))
            .stdin(Stdio::null());
        cmd
    }

    /// `ddc disk free --actions - <args>` with `list` on stdin.
    fn free(&self, list: &str, args: &[&str]) -> Output {
        let mut all = vec!["free", "--actions", "-"];
        all.extend_from_slice(args);
        let mut child =
            self.ddc(&all).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        child.stdin.take().unwrap().write_all(list.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    fn calls(&self, tool: &str) -> Vec<String> {
        fs::read_to_string(self.root.join(format!("{tool}-calls")))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn journal(&self) -> Vec<serde_json::Value> {
        fs::read_to_string(self.root.join("data/logs/actions.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn env(&self) -> Env {
        Env::with_paths(
            &self.root.join("home"),
            &self.root.join("data"),
            &self.root.join("data/logs"),
            SystemTime::now(),
        )
        .unwrap()
    }
}

const RUNTIME: &str = r#"{"schema_version": 1, "actions": [{"action": "simctl-runtime-delete", "identifier": "A"}]}"#;
const BREW: &str = r#"{"schema_version": 1, "actions": [{"action": "brew-cleanup"}]}"#;

fn report(out: &Output) -> FreeReport {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("{e}: {}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    })
}

#[test]
fn without_actions_and_without_a_terminal_it_refuses_and_runs_nothing() {
    let fx = Fx::new();
    let out = fx.ddc(&["free"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("needs a terminal"));
    assert!(fx.calls("simctl").is_empty() && fx.calls("brew").is_empty());
    assert!(fx.journal().is_empty());
}

#[test]
fn there_is_no_yes() {
    let fx = Fx::new();
    let out = fx.ddc(&["free", "--yes"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(fx.calls("simctl").is_empty() && fx.calls("brew").is_empty());
}

#[test]
fn a_malformed_list_is_refused_whole_before_anything_runs() {
    let fx = Fx::new();
    for list in [
        r#"{"schema_version": 1, "actions": [{"action": "brew-cleanup", "now": true}]}"#,
        r#"{"schema_version": 1, "actions": [{"action": "brew-cleanup"}, {"action": "simctl-runtime-delete"}]}"#,
        r#"{"schema_version": 1, "actions": [{"action": "brew-cleanup"}, {"action": "brew-cleanup"}]}"#,
        "not json",
    ] {
        let out = fx.free(list, &["--json"]);
        assert_eq!(out.status.code(), Some(2), "{list}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(out.stdout.is_empty());
    }
    assert!(fx.calls("simctl").is_empty() && fx.calls("brew").is_empty());
    assert!(!fx.root.join("data/logs").exists(), "no journal before a list is accepted");
}

#[test]
fn a_confirmed_runtime_is_deleted_journaled_and_left_out_of_the_saved_disk_report() {
    let fx = Fx::new();
    let disk = fx.ddc(&["--json"]).output().unwrap();
    assert!(disk.status.success(), "{}", String::from_utf8_lossy(&disk.stderr));
    // Only the run's calls count.
    fs::write(fx.root.join("simctl-calls"), "").unwrap();

    let out = fx.free(RUNTIME, &["--json"]);

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let r = report(&out);
    assert!(!r.dry_run);
    assert_eq!(r.actions[0].result, ActionResult::Freed);
    assert!(r.actions[0].bytes_before.unwrap() >= 300_000);
    assert_eq!(r.actions[0].bytes_after, Some(0));
    let deletes: Vec<String> = fx.calls("simctl").into_iter().filter(|c| c.contains("delete")).collect();
    assert_eq!(deletes, [format!("runtime delete A | {}/sys/Dev", fx.root.display())]);

    let journal = fx.journal();
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0]["schema_version"], 2);
    assert_eq!(journal[0]["kind"], "area-action");
    assert_eq!(journal[0]["result"]["status"], "freed");
    assert!(journal[0]["command"].as_str().unwrap().ends_with("simctl runtime delete A"));

    assert_eq!(r.last_disk, ReportEdit::Edited);
    let saved = store::read_last_disk(&fx.env()).unwrap().unwrap();
    assert!(saved.areas.iter().find(|a| a.id == "simulator-runtimes").unwrap().stale);
    assert!(saved.simulators.unwrap().runtimes.is_empty());
}

#[test]
fn brew_cleanup_runs_alone_with_no_autoremove_and_its_dry_run_changes_nothing() {
    let fx = Fx::new();
    let dry = fx.free(BREW, &["--dry-run", "--json"]);
    assert!(dry.status.success(), "{}", String::from_utf8_lossy(&dry.stderr));
    let r = report(&dry);
    assert!(r.dry_run && r.journal.is_none());
    assert_eq!(r.actions[0].result, ActionResult::WouldRun);
    assert_eq!(r.actions[0].preview.as_ref().unwrap().estimate_bytes, Some(39 * 1024));
    assert!(fx.calls("brew").contains(&"cleanup -n | 1 1 1".to_owned()), "{:?}", fx.calls("brew"));
    assert!(!fx.calls("brew").iter().any(|c| c.starts_with("cleanup |")));
    assert!(fx.journal().is_empty());
    assert!(fx.root.join("home/Library/Caches/Homebrew/downloads").exists());

    let out = fx.free(BREW, &["--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(report(&out).actions[0].result, ActionResult::Freed);
    let runs: Vec<String> = fx.calls("brew").into_iter().filter(|c| c.starts_with("cleanup ")).collect();
    assert_eq!(runs, ["cleanup -n | 1 1 1", "cleanup | 1 1 1"]);
    assert_eq!(fx.journal().len(), 1);
}

#[test]
fn after_brew_cleanup_the_last_scan_is_measured_again() {
    let fx = Fx::new();
    let disk = fx.ddc(&["--json"]).output().unwrap();
    assert!(disk.status.success());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"));
    let scanned = cmd
        .env_clear()
        .env("HOME", fx.root.join("home"))
        .env("PATH", "")
        .args(["scan", "--json", "--data-dir"])
        .arg(fx.root.join("data"))
        .output()
        .unwrap();
    assert!(scanned.status.success(), "{}", String::from_utf8_lossy(&scanned.stderr));
    let before = store::read_last_scan(&fx.env()).unwrap().unwrap();
    assert!(before.findings.iter().any(|f| f.rule == "cache.homebrew"), "{:?}", before.findings);

    let out = fx.free(BREW, &["--json"]);

    let r = report(&out);
    assert_eq!(r.last_scan, ReportEdit::Edited);
    let after = store::read_last_scan(&fx.env()).unwrap().unwrap();
    let old = before.findings.iter().find(|f| f.rule == "cache.homebrew").unwrap().bytes;
    assert!(after.findings.iter().find(|f| f.rule == "cache.homebrew").is_none_or(|f| f.bytes < old));
    assert_eq!(r.last_disk, ReportEdit::Edited);
    let disk = store::read_last_disk(&fx.env()).unwrap().unwrap();
    assert!(disk.areas.iter().find(|a| a.id == "homebrew").unwrap().stale);
}

#[test]
fn a_permission_error_fails_with_the_terminal_command_and_exits_1_in_text() {
    let fx = Fx::new();
    fs::write(fx.root.join("brew-denied"), "").unwrap();

    let json = fx.free(BREW, &["--json"]);
    assert_eq!(json.status.code(), Some(0), "with --json the report carries the failure");
    let a = &report(&json).actions[0];
    assert_eq!(a.result, ActionResult::Failed { needs_admin: true });
    assert_eq!(
        a.terminal_command,
        format!(
            "HOMEBREW_NO_AUTOREMOVE=1 HOMEBREW_NO_ENV_HINTS=1 NO_COLOR=1 {}/sys/opt/homebrew/bin/brew cleanup",
            fx.root.display()
        )
    );

    let text = fx.free(BREW, &[]);
    assert_eq!(text.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(stdout.contains("needs an administrator") && stdout.contains(&a.terminal_command), "{stdout}");
}

#[test]
fn a_refused_action_runs_nothing_and_the_run_still_exits_0() {
    let fx = Fx::new();
    let list = r#"{"schema_version": 1, "actions": [{"action": "simctl-runtime-delete", "identifier": "all"}]}"#;
    let out = fx.free(list, &["--json"]);
    assert!(out.status.success());
    let r = report(&out);
    assert_eq!(r.actions[0].result, ActionResult::Refused { reason: Refusal::UnknownRuntime });
    assert_eq!(r.last_disk, ReportEdit::Unchanged);
    assert!(!fx.calls("simctl").iter().any(|c| c.contains("delete")));
}

#[test]
fn a_held_lock_exits_75_and_runs_nothing() {
    let fx = Fx::new();
    let env = fx.env();
    let held = store::try_lock(&env, LockMode::Shared).unwrap();
    let out = fx.free(BREW, &["--json"]);
    assert_eq!(out.status.code(), Some(75));
    assert!(!fx.calls("brew").iter().any(|c| c.starts_with("cleanup")));
    drop(held);

    // The dry run shares the lock with a scan.
    let _scan = store::try_lock(&env, LockMode::Shared).unwrap();
    assert!(fx.free(BREW, &["--dry-run", "--json"]).status.success());
}
