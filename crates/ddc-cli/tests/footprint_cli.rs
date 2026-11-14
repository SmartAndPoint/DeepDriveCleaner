//! `ddc agent footprint` end to end (story "ddc footprint: a scoped project
//! report"): a temporary home with a real repository and Claude Code
//! worktrees, made with the `git` on `PATH`. It takes no lock, saves no
//! report and removes nothing.

#![cfg(unix)]

use ddc_core::platform::Canonical as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::SystemTime;

use ddc_core::env::Env;
use ddc_core::footprint::{Bucket, FootprintReport};
use ddc_core::holders::HolderCommand;
use ddc_core::refusal::ErrorReport;
use ddc_core::store::{self, LockMode};
use ddc_loadgen::isolate::ClearEnv as _;

mod common;
mod contract;

struct Fx {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    data: PathBuf,
}

impl Fx {
    /// A home with `proj` (a commit, `.gitignore`) and a Claude Code
    /// worktree `a` of it; `None` without a git.
    fn ready(test: &str) -> Option<(Self, PathBuf, PathBuf)> {
        if common::unsupported(test) {
            return None;
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonical().unwrap();
        let (home, data) = (root.join("home"), root.join("data"));
        let main = home.join("proj");
        fs::create_dir_all(&main).unwrap();
        fs::create_dir_all(&data).unwrap();
        common::skipped_without(test, common::git(&main, &["init", "-q", "-b", "main"]))?;
        fs::write(main.join(".gitignore"), ".venv/\n").unwrap();
        fs::write(main.join("README"), "hello\n").unwrap();
        common::git(&main, &["add", "."]).unwrap();
        common::git(&main, &["commit", "-q", "-m", "init"]).unwrap();
        let a = main.join(".claude/worktrees/a");
        common::git(&main, &["worktree", "add", "-q", "-b", "worktree-a", a.to_str().unwrap()]).unwrap();
        Some((Self { _tmp: tmp, home, data }, main, a))
    }

    #[allow(clippy::disallowed_methods)] // the test's own PATH, for the git the fixture used
    fn ddc(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
            .clear_env()
            .home_env(&self.home)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("NO_COLOR", "1")
            .args(["agent", "footprint"])
            .args(args)
            .arg("--data-dir")
            .arg(&self.data)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_worktree_resolves_to_its_project_and_the_report_holds_its_schema() {
    let Some((fx, main, a)) = Fx::ready("a_worktree_resolves_to_its_project_and_the_report_holds_its_schema") else {
        return;
    };
    // Used now: within the two-day window, it waits; with no window, it is selected.
    let out = fx.ddc(&[a.to_str().unwrap(), "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let r: FootprintReport = contract::holds("footprint", &out.stdout);
    assert_eq!(r.main, main);
    assert_eq!(r.checkouts.len(), 2);
    let entry = r.checkouts.iter().find(|c| c.path == a).unwrap();
    assert_eq!(entry.bucket, Some(Bucket::AfterSessions));
    assert_eq!(r.totals.reclaimable_after_sessions, entry.bytes);

    let out = fx.ddc(&[a.to_str().unwrap(), "--json", "--worktree-min-age", "0"]);
    let r: FootprintReport = contract::holds("footprint", &out.stdout);
    let entry = r.checkouts.iter().find(|c| c.path == a).unwrap();
    assert_eq!(entry.bucket, Some(Bucket::Now), "--worktree-min-age 0 moves it to now");
    assert_eq!(r.totals.reclaimable_now.rebuildable, entry.bytes);

    let out = fx.ddc(&[main.to_str().unwrap(), "--worktree-min-age", "0", "--min-age", "14"]);
    assert!(out.status.success());
    let shown = text(&out);
    assert!(shown.contains("main checkout"), "{shown}");
    assert!(
        shown.contains("Run `ddc clean ~/proj` to review and remove what is selected."),
        "{shown}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn it_takes_no_lock_and_saves_nothing() {
    let Some((fx, main, _)) = Fx::ready("it_takes_no_lock_and_saves_nothing") else { return };
    let env = Env::with_paths(&fx.home, &fx.data, &fx.data.join("logs"), SystemTime::now()).unwrap();
    let _held = store::try_lock(&env, LockMode::Exclusive, HolderCommand::Clean).unwrap();
    let out = fx.ddc(&[main.to_str().unwrap(), "--json"]);
    assert!(out.status.success(), "a held lock does not stop it: {}", String::from_utf8_lossy(&out.stderr));
    assert!(!fx.data.join(store::LAST_SCAN).exists(), "no report is saved");
}

#[test]
fn outside_a_repository_it_refuses_with_exit_2() {
    let Some((fx, _, _)) = Fx::ready("outside_a_repository_it_refuses_with_exit_2") else { return };
    let plain = fx.home.join("plain");
    fs::create_dir_all(&plain).unwrap();
    assert!(
        !plain.ancestors().any(|a: &Path| a.join(".git").exists()),
        "the temporary folder lies inside a repository"
    );
    let out = fx.ddc(&[plain.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).contains("is not a folder inside a git repository"));

    let out = fx.ddc(&[plain.to_str().unwrap(), "--json"]);
    assert_eq!(out.status.code(), Some(2));
    let report: ErrorReport = contract::holds("error", &out.stdout);
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["error"]["code"], "not-a-repository");
    assert_eq!(json["error_schema_version"], 5);

    // The home itself as the main checkout: a home scan, refused.
    common::git(&fx.home, &["init", "-q", "-b", "main"]).unwrap();
    let out = fx.ddc(&[plain.to_str().unwrap(), "--json"]);
    assert_eq!(out.status.code(), Some(2));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["error"]["code"], "home-is-the-checkout");
}
