//! `ddc disk free` end to end (story "Whole disk: free space with the owning
//! tool"): a temporary HOME and data directory, the system's folders and
//! tools under `--system-root`, and an empty `PATH`. `simctl`, `xcodebuild`,
//! `xcode-select` and `brew` are shell scripts that log their calls and
//! touch only the fixture; nothing real is run.

use std::fs;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::SystemTime;

use ddc_core::env::Env;
use ddc_core::free::{ActionResult, FreeReport, Refusal, ReportEdit};
use ddc_core::store::{self, LockMode};
use ddc_loadgen::isolate::ClearEnv as _;

mod common;
mod contract;

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
        common::symlink("System/Volumes/Data/opt", fx.root.join("sys/opt"));
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
        common::set_mode(&path, 0o755);
    }

    fn ddc(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"));
        cmd.clear_env()
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

const RUNTIME: &str = r#"{"schema_version": 3, "actions": [{"action": "simctl-runtime-delete", "identifier": "A"}]}"#;
const BREW: &str = r#"{"schema_version": 3, "actions": [{"action": "brew-cleanup"}]}"#;

fn report(out: &Output) -> FreeReport {
    assert!(!out.stdout.is_empty(), "no report: {}", String::from_utf8_lossy(&out.stderr));
    contract::holds("disk-free", &out.stdout)
}

#[test]
fn without_actions_and_without_a_terminal_it_refuses_and_runs_nothing() {
    if common::unsupported("without_actions_and_without_a_terminal_it_refuses_and_runs_nothing") {
        return;
    }
    let fx = Fx::new();
    let out = fx.ddc(&["free"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("needs a terminal"));
    assert!(fx.calls("simctl").is_empty() && fx.calls("brew").is_empty());
    assert!(fx.journal().is_empty());
}

#[test]
fn there_is_no_yes() {
    if common::unsupported("there_is_no_yes") {
        return;
    }
    let fx = Fx::new();
    let out = fx.ddc(&["free", "--yes"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(fx.calls("simctl").is_empty() && fx.calls("brew").is_empty());
}

#[test]
fn a_malformed_list_is_refused_whole_before_anything_runs() {
    if common::unsupported("a_malformed_list_is_refused_whole_before_anything_runs") {
        return;
    }
    let fx = Fx::new();
    for list in [
        r#"{"schema_version": 3, "actions": [{"action": "brew-cleanup", "now": true}]}"#,
        r#"{"schema_version": 3, "actions": [{"action": "brew-cleanup"}, {"action": "simctl-runtime-delete"}]}"#,
        r#"{"schema_version": 3, "actions": [{"action": "brew-cleanup"}, {"action": "brew-cleanup"}]}"#,
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
    if common::unsupported("a_confirmed_runtime_is_deleted_journaled_and_left_out_of_the_saved_disk_report") {
        return;
    }
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
    assert_eq!(journal[0]["schema_version"], 5);
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
    if common::unsupported("brew_cleanup_runs_alone_with_no_autoremove_and_its_dry_run_changes_nothing") {
        return;
    }
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
    if common::unsupported("after_brew_cleanup_the_last_scan_is_measured_again") {
        return;
    }
    let fx = Fx::new();
    let disk = fx.ddc(&["--json"]).output().unwrap();
    assert!(disk.status.success());
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"));
    let scanned = cmd
        .clear_env()
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
    if common::unsupported("a_permission_error_fails_with_the_terminal_command_and_exits_1_in_text") {
        return;
    }
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
    if common::unsupported("a_refused_action_runs_nothing_and_the_run_still_exits_0") {
        return;
    }
    let fx = Fx::new();
    let list = r#"{"schema_version": 3, "actions": [{"action": "simctl-runtime-delete", "identifier": "all"}]}"#;
    let out = fx.free(list, &["--json"]);
    assert!(out.status.success());
    let r = report(&out);
    assert_eq!(r.actions[0].result, ActionResult::Refused { reason: Refusal::UnknownRuntime });
    assert_eq!(r.last_disk, ReportEdit::Unchanged);
    assert!(!fx.calls("simctl").iter().any(|c| c.contains("delete")));
}

#[test]
fn a_held_lock_exits_75_and_runs_nothing() {
    if common::unsupported("a_held_lock_exits_75_and_runs_nothing") {
        return;
    }
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

// Story "Whole disk: Conda environments and its package cache, with conda":
// an `/opt/miniconda3` whose `bin/conda` is a shell script that logs each
// call with its `CONDA_PKGS_DIRS` and touches only the fixture.

const SHOW: &str = "config --show root_prefix envs_dirs pkgs_dirs allow_softlinks always_softlink --json";
const SHOW_PKGS: &str = "config --show pkgs_dirs --json";
const CLEAN: &str = "clean --index-cache --packages --tarballs --logfiles --yes";
const CLEAN_DRY_RUN: &str = "clean --index-cache --packages --tarballs --logfiles --dry-run --json";

impl Fx {
    /// The install, its environment `old` (with its own file and one
    /// hard-linked from `pkgs`) and `pkgs` with a package no environment uses.
    fn with_conda(self) -> Self {
        let r = self.root.display().to_string();
        let data = "sys/System/Volumes/Data/opt/miniconda3";
        self.write(&format!("{data}/conda-meta/history"), 0);
        self.write(&format!("{data}/envs/old/conda-meta/history"), 0);
        self.write(&format!("{data}/envs/old/lib/own.dylib"), 64_000);
        self.write(&format!("{data}/pkgs/numpy/lib/numpy.so"), 128_000);
        self.write(&format!("{data}/pkgs/scipy/lib/scipy.so"), 32_000);
        fs::hard_link(
            self.root.join(format!("{data}/pkgs/numpy/lib/numpy.so")),
            self.root.join(format!("{data}/envs/old/lib/numpy.so")),
        )
        .unwrap();
        let prefix = format!("{r}/sys/opt/miniconda3");
        let settings = format!(
            r#"{{"root_prefix": "{prefix}", "envs_dirs": ["{prefix}/envs"], "pkgs_dirs": ["{prefix}/pkgs"], "allow_softlinks": false, "always_softlink": false}}"#
        );
        self.tool(
            &format!("{data}/bin/conda"),
            &format!(
                r#"echo "$* | $CONDA_PKGS_DIRS" >> {r}/conda-calls
case "$*" in
  "{SHOW}") echo '{settings}' ;;
  "{SHOW_PKGS}") echo '{{"pkgs_dirs": ["{prefix}/pkgs"]}}' ;;
  "{CLEAN_DRY_RUN}") echo '{{"packages": {{"total_size": 32000}}, "tarballs": {{"total_size": 1000}}}}'
    echo "DryRunExit: Dry run. Exiting." ;;
  "{CLEAN}") /bin/rm -rf {prefix}/pkgs/scipy ;;
  "env remove -p {prefix}/envs/old --yes") /bin/rm -rf {prefix}/envs/old ;;
  *) exit 1 ;;
esac"#
            ),
        );
        self
    }

    fn opt_conda(&self) -> String {
        format!("{}/sys/opt/miniconda3", self.root.display())
    }

    fn conda_list(&self, action: &str, identifier: &str) -> String {
        serde_json::json!({"schema_version": 3, "actions": [{"action": action, "identifier": identifier}]}).to_string()
    }
}

#[test]
fn a_confirmed_opt_environment_is_removed_by_its_own_conda_and_left_out_of_the_saved_disk_report() {
    if common::unsupported(
        "a_confirmed_opt_environment_is_removed_by_its_own_conda_and_left_out_of_the_saved_disk_report",
    ) {
        return;
    }
    let fx = Fx::new().with_conda();
    let disk = fx.ddc(&["--json"]).output().unwrap();
    assert!(disk.status.success(), "{}", String::from_utf8_lossy(&disk.stderr));
    assert!(fx.calls("conda").is_empty(), "the disk report runs no conda");
    let saved = store::read_last_disk(&fx.env()).unwrap().unwrap();
    let env = format!("{}/envs/old", fx.opt_conda());
    let listed = &saved.conda.as_ref().unwrap().installs[0].environments;
    assert_eq!(listed.iter().map(|e| e.path.display().to_string()).collect::<Vec<_>>(), std::slice::from_ref(&env));
    assert!(listed[0].writable && listed[0].shared_bytes > 0);

    let out = fx.free(&fx.conda_list("conda-env-remove", &env), &["--json"]);

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let r = report(&out);
    assert_eq!(r.actions[0].result, ActionResult::Freed, "{:?}", r.actions[0]);
    let before = r.actions[0].bytes_before.unwrap();
    assert!((64_000..128_000).contains(&before), "only its own bytes: {before}");
    assert_eq!(fx.calls("conda"), [format!("{SHOW} | "), format!("env remove -p {env} --yes | ")]);
    let journal = fx.journal();
    assert_eq!(journal.len(), 1);
    assert_eq!(journal[0]["action"], "conda-env-remove");
    assert_eq!(journal[0]["command"], format!("{}/bin/conda env remove -p {env} --yes", fx.opt_conda()));

    assert_eq!(r.last_disk, ReportEdit::Edited);
    let saved = store::read_last_disk(&fx.env()).unwrap().unwrap();
    assert!(saved.areas.iter().find(|a| a.id == "conda").unwrap().stale);
    assert!(saved.conda.unwrap().installs[0].environments.is_empty());
    assert!(fx.root.join("sys/System/Volumes/Data/opt/miniconda3/pkgs/numpy/lib/numpy.so").exists());
}

#[test]
fn conda_clean_is_limited_to_the_package_folder_and_its_dry_run_changes_nothing() {
    if common::unsupported("conda_clean_is_limited_to_the_package_folder_and_its_dry_run_changes_nothing") {
        return;
    }
    let fx = Fx::new().with_conda();
    let pkgs = format!("{}/pkgs", fx.opt_conda());
    let list = fx.conda_list("conda-clean", &fx.opt_conda());

    let dry = fx.free(&list, &["--dry-run", "--json"]);
    assert!(dry.status.success(), "{}", String::from_utf8_lossy(&dry.stderr));
    let r = report(&dry);
    assert_eq!(r.actions[0].result, ActionResult::WouldRun, "{:?}", r.actions[0]);
    assert_eq!(r.actions[0].preview.as_ref().unwrap().estimate_bytes, Some(33_000));
    assert!(fx.calls("conda").contains(&format!("{CLEAN_DRY_RUN} | {pkgs}")), "{:?}", fx.calls("conda"));
    assert!(!fx.calls("conda").iter().any(|c| c.starts_with(&format!("{CLEAN} "))));
    assert!(fx.journal().is_empty());

    let out = fx.free(&list, &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let runs: Vec<String> = fx.calls("conda").into_iter().filter(|c| c.starts_with("clean")).collect();
    assert_eq!(runs, [format!("{CLEAN_DRY_RUN} | {pkgs}"), format!("{CLEAN} | {pkgs}")]);
    assert!(fx.calls("conda").contains(&format!("{SHOW_PKGS} | {pkgs}")));
    assert!(!fx.root.join("sys/System/Volumes/Data/opt/miniconda3/pkgs/scipy").exists());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains(&format!("Conda package cache of {}", fx.opt_conda())), "{text}");
}

#[test]
fn a_home_environment_is_refused_in_words_and_runs_no_conda() {
    if common::unsupported("a_home_environment_is_refused_in_words_and_runs_no_conda") {
        return;
    }
    let fx = Fx::new().with_conda();
    fx.write("home/miniconda3/envs/x/conda-meta/history", 0);
    let home = format!("{}/home/miniconda3/envs/x", fx.root.display());
    let out = fx.free(&fx.conda_list("conda-env-remove", &home), &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("refused: it is in the home folder: the Review window cleans it"), "{text}");
    assert!(fx.calls("conda").is_empty());
}

#[test]
fn a_conda_action_under_a_held_lock_exits_75_and_runs_no_conda() {
    if common::unsupported("a_conda_action_under_a_held_lock_exits_75_and_runs_no_conda") {
        return;
    }
    let fx = Fx::new().with_conda();
    let held = store::try_lock(&fx.env(), LockMode::Shared).unwrap();
    let env = format!("{}/envs/old", fx.opt_conda());
    let out = fx.free(&fx.conda_list("conda-env-remove", &env), &["--json"]);
    assert_eq!(out.status.code(), Some(75));
    assert!(fx.calls("conda").is_empty());
    assert!(fx.root.join("sys/System/Volumes/Data/opt/miniconda3/envs/old").exists());
    drop(held);
}

// Story "Whole disk: free the checked areas in one sheet": the app's checked
// list is one `--actions` list of several actions, run in order under one lock.

const ASSET_B: &str = "sys/System/Library/AssetsV2/com_apple_MobileAsset_iOSSimulatorRuntime/b.asset";
const TWO_RUNTIMES: &str = r#"{"schema_version": 3, "actions": [{"action": "simctl-runtime-delete", "identifier": "A"}, {"action": "simctl-runtime-delete", "identifier": "B"}]}"#;

impl Fx {
    /// A second downloaded runtime, `B`, in its own asset folder beside
    /// `A`'s; simctl lists each until its own `deleted-<id>` marker exists.
    fn with_two_runtimes(self) -> Self {
        let r = self.root.display().to_string();
        self.write(&format!("{ASSET_B}/AssetData/runtime.dmg"), 200_000);
        let runtime = |id: &str, version: &str, size: u32, asset: &str| {
            let data = format!("{r}/{asset}/AssetData");
            format!(
                r#""{id}": {{"identifier": "{id}", "runtimeIdentifier": "com.apple.CoreSimulator.SimRuntime.iOS-{}", "version": "{version}", "sizeBytes": {size}, "deletable": true, "state": "Ready", "path": "{data}", "parentMountPath": "{data}"}}"#,
                version.replace('.', "-")
            )
        };
        let a = runtime("A", "27.1", 300_000, ASSET);
        let b = runtime("B", "26.0", 200_000, ASSET_B);
        self.tool(
            &format!("sys/{SIMCTL}"),
            &format!(
                r#"echo "$* | $DEVELOPER_DIR" >> {r}/simctl-calls
case "$*" in
  "runtime list -j")
    sep=""
    printf '{{'
    if [ ! -f {r}/deleted-A ]; then printf '%s' '{a}'; sep=","; fi
    if [ ! -f {r}/deleted-B ]; then printf '%s%s' "$sep" '{b}'; fi
    echo '}}' ;;
  "list devices -j") echo '{{"devices": {{}}}}' ;;
  "runtime delete A --dry-run") echo "Would delete runtime A" ;;
  "runtime delete B --dry-run") echo "Would delete runtime B" ;;
  "runtime delete A") : > {r}/deleted-A; /bin/rm -rf {r}/{ASSET} ;;
  "runtime delete B") : > {r}/deleted-B; /bin/rm -rf {r}/{ASSET_B} ;;
  *) exit 1 ;;
esac"#
            ),
        );
        self
    }
}

#[test]
fn two_runtimes_in_one_list_run_in_order_each_journaled_and_both_leave_the_saved_disk_report() {
    if common::unsupported("two_runtimes_in_one_list_run_in_order_each_journaled_and_both_leave_the_saved_disk_report")
    {
        return;
    }
    let fx = Fx::new().with_two_runtimes();
    let disk = fx.ddc(&["--json"]).output().unwrap();
    assert!(disk.status.success(), "{}", String::from_utf8_lossy(&disk.stderr));
    let before = store::read_last_disk(&fx.env()).unwrap().unwrap().simulators.unwrap();
    let listed: Vec<&str> = before.runtimes.iter().map(|rt| rt.identifier.as_str()).collect();
    assert_eq!(listed.len(), 2, "{listed:?}");
    assert_eq!(before.listed_bytes, 500_000);
    fs::write(fx.root.join("simctl-calls"), "").unwrap();

    // The sheet's dry run: both in one call, each would run, nothing changes.
    let dry = fx.free(TWO_RUNTIMES, &["--dry-run", "--json"]);
    assert!(dry.status.success(), "{}", String::from_utf8_lossy(&dry.stderr));
    let preview = report(&dry);
    let ids: Vec<Option<&str>> = preview.actions.iter().map(|a| a.identifier.as_deref()).collect();
    assert_eq!(ids, [Some("A"), Some("B")]);
    assert!(preview.actions.iter().all(|a| a.result == ActionResult::WouldRun));
    assert!(fx.journal().is_empty() && !fx.root.join("deleted-A").exists() && !fx.root.join("deleted-B").exists());

    let out = fx.free(TWO_RUNTIMES, &["--json"]);

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let r = report(&out);
    let ids: Vec<Option<&str>> = r.actions.iter().map(|a| a.identifier.as_deref()).collect();
    assert_eq!(ids, [Some("A"), Some("B")], "one report per action, in the list's order");
    assert!(r.actions.iter().all(|a| a.result == ActionResult::Freed), "{:?}", r.actions);
    let root = fx.root.display();
    let deletes: Vec<String> =
        fx.calls("simctl").into_iter().filter(|c| c.contains("delete") && !c.contains("dry-run")).collect();
    assert_eq!(deletes, [format!("runtime delete A | {root}/sys/Dev"), format!("runtime delete B | {root}/sys/Dev")]);

    let journal = fx.journal();
    assert_eq!(journal.len(), 2, "one line per action");
    for (line, id) in journal.iter().zip(["A", "B"]) {
        assert_eq!(line["kind"], "area-action");
        assert_eq!(line["identifier"], id);
        assert_eq!(line["result"]["status"], "freed");
    }

    assert_eq!(r.last_disk, ReportEdit::Edited);
    let saved = store::read_last_disk(&fx.env()).unwrap().unwrap();
    assert!(saved.areas.iter().find(|a| a.id == "simulator-runtimes").unwrap().stale);
    let sims = saved.simulators.unwrap();
    assert!(sims.runtimes.is_empty(), "both left the list");
    assert_eq!(sims.listed_bytes, 0, "the sum of those that remain");
}
