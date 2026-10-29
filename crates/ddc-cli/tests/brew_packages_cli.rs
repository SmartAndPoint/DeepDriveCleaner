//! `ddc disk brew-packages` end to end (story "Whole disk: Homebrew
//! packages, their sizes, and brew uninstall"): a temporary HOME and data
//! directory, the Homebrew area under `--system-root`, and an empty `PATH`.
//! `brew` is a shell script that logs its calls and their environment and
//! prints a fixture; nothing real is run.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use ddc_core::brew::{BREW_PACKAGES_SCHEMA_VERSION, BrewPackages, NotOffered, ServicesState};
use ddc_core::free::{ActionResult, FreeReport, Refusal};
use serde_json::json;

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let fx = Self { root: tmp.path().canonicalize().unwrap(), _tmp: tmp };
        let r = fx.root.display().to_string();
        fx.write("sys/System/Volumes/Data/opt/homebrew/Cellar/wget/1.25.0/bin/wget", 40_000);
        fx.write("sys/System/Volumes/Data/opt/homebrew/Cellar/openssl@3/3.4.0/lib/libssl.dylib", 20_000);
        fs::create_dir_all(fx.root.join("sys/System/Volumes/Data/opt/homebrew/Library/Homebrew/services")).unwrap();
        fs::create_dir_all(fx.root.join("home/Library/Caches/Homebrew")).unwrap();
        std::os::unix::fs::symlink("System/Volumes/Data/opt", fx.root.join("sys/opt")).unwrap();
        let keg = |v: &str, on_request: bool, deps: &[&str]| {
            json!({"version": v, "installed_on_request": on_request,
                   "runtime_dependencies": deps.iter().map(|d| json!({"full_name": d})).collect::<Vec<_>>()})
        };
        let info = json!({
            "formulae": [
                {"name": "wget", "full_name": "wget", "pinned": false, "installed": [keg("1.25.0", true, &["openssl@3"])]},
                {"name": "openssl@3", "full_name": "openssl@3", "pinned": false, "installed": [keg("3.4.0", false, &[])]},
                {"name": "curl", "full_name": "curl", "pinned": false, "installed": [keg("8.11.0", true, &["openssl@3"])]},
            ],
            "casks": [],
        });
        fs::write(fx.root.join("info.json"), info.to_string()).unwrap();
        fx.tool(
            "sys/System/Volumes/Data/opt/homebrew/bin/brew",
            &format!(
                r#"echo "$* | $HOMEBREW_NO_AUTOREMOVE $HOMEBREW_NO_ENV_HINTS $NO_COLOR $HOMEBREW_NO_AUTO_UPDATE" >> {r}/brew-calls
case "$*" in
  --prefix) if [ -f {r}/elsewhere ]; then echo /usr/local; else echo {r}/sys/opt/homebrew; fi ;;
  --cellar) echo {r}/sys/opt/homebrew/Cellar ;;
  --cache) echo {r}/home/Library/Caches/Homebrew ;;
  "info --json=v2 --installed") if [ -f {r}/offline ]; then echo "Error: Failure while executing curl" >&2; exit 1; fi
    /bin/cat {r}/info.json ;;
  "services list --json") echo '[]' ;;
  "uninstall --formula wget") /bin/mv {r}/sys/System/Volumes/Data/opt/homebrew/Cellar/wget {r}/uninstalled ;;
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

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("PATH", "")
            .env("NO_COLOR", "1")
            .args(["disk", "brew-packages"])
            .args(args)
            .arg("--data-dir")
            .arg(self.root.join("data"))
            .arg("--system-root")
            .arg(self.root.join("sys"))
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    /// `ddc disk free --actions - <args>` with `list` on stdin.
    fn free(&self, list: &str, args: &[&str]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
            .env_clear()
            .env("HOME", self.root.join("home"))
            .env("PATH", "")
            .env("NO_COLOR", "1")
            .args(["disk", "free", "--actions", "-"])
            .args(args)
            .arg("--data-dir")
            .arg(self.root.join("data"))
            .arg("--system-root")
            .arg(self.root.join("sys"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(list.as_bytes()).unwrap();
        child.wait_with_output().unwrap()
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.root.join("brew-calls")).unwrap_or_default().lines().map(str::to_owned).collect()
    }
}

#[test]
fn the_listing_runs_only_brews_read_commands_and_prints_its_json() {
    let fx = Fx::new();
    let out = fx.run(&["--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let listed: BrewPackages = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(listed.schema_version, BREW_PACKAGES_SCHEMA_VERSION);
    assert_eq!(listed.services, ServicesState::Listed);
    let names: Vec<&str> = listed.formulae.iter().map(|p| p.full_name.as_str()).collect();
    assert_eq!(names, ["wget", "curl"], "on request only, largest first");
    let wget = &listed.formulae[0];
    assert!(wget.bytes.unwrap() >= 40_000);
    assert_eq!(wget.not_offered, None);
    assert!(wget.would_orphan.is_empty(), "curl needs openssl@3 too");
    let calls = fx.calls();
    assert_eq!(
        calls,
        [
            "--prefix | 1 1 1 1",
            "--cellar | 1 1 1 1",
            "--cache | 1 1 1 1",
            "info --json=v2 --installed | 1 1 1 1",
            "services list --json | 1 1 1 1",
        ]
    );
    // Nothing saved: no lock, no report, no journal.
    assert!(!fx.root.join("data/logs/actions.jsonl").exists());
    assert!(!fx.root.join("data/last-disk.json").exists());
}

#[test]
fn the_text_listing_names_each_formula_and_why_one_is_not_offered() {
    let fx = Fx::new();
    let mut info: serde_json::Value = serde_json::from_slice(&fs::read(fx.root.join("info.json")).unwrap()).unwrap();
    info["formulae"][1]["installed"][0]["installed_on_request"] = json!(true);
    fs::write(fx.root.join("info.json"), info.to_string()).unwrap();
    let out = fx.run(&[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.lines().any(|l| l.starts_with("wget")), "{text}");
    assert!(text.lines().any(|l| l.starts_with("openssl@3") && l.ends_with("needed by curl, wget")), "{text}");

    let listed: BrewPackages = serde_json::from_slice(&fx.run(&["--json"]).stdout).unwrap();
    let openssl = listed.formulae.iter().find(|p| p.full_name == "openssl@3").unwrap();
    assert_eq!(openssl.not_offered, Some(NotOffered::Needed));
}

#[test]
fn a_brew_that_cannot_list_exits_1_and_says_why() {
    let fx = Fx::new();
    fs::write(fx.root.join("offline"), "").unwrap();
    let out = fx.run(&["--json"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.starts_with("ddc: brew could not list its packages: "), "{err}");
    assert!(err.contains("Failure while executing curl"), "{err}");
}

#[test]
fn a_brew_elsewhere_is_not_asked_for_its_packages() {
    let fx = Fx::new();
    fs::write(fx.root.join("elsewhere"), "").unwrap();
    let out = fx.run(&["--json"]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("brew --prefix is not /opt/homebrew"), "{err}");
    assert!(!fx.calls().iter().any(|c| c.starts_with("info")));
}

/// The list the app's sheet writes for a formula of the listing.
fn uninstall(listed: &BrewPackages, full_name: &str) -> String {
    let p = listed.formulae.iter().find(|p| p.full_name == full_name).unwrap();
    let kegs: Vec<&str> = p.kegs.iter().map(|k| k.version.as_str()).collect();
    json!({"schema_version": 3, "actions": [{"action": "brew-uninstall", "identifier": full_name, "kegs": kegs}]})
        .to_string()
}

fn free_report(out: &Output) -> FreeReport {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("{e}: {}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
    })
}

#[test]
fn a_listed_formula_is_uninstalled_by_brew_only_after_the_check() {
    let fx = Fx::new();
    let listed: BrewPackages = serde_json::from_slice(&fx.run(&["--json"]).stdout).unwrap();
    let list = uninstall(&listed, "wget");
    let rack = fx.root.join("sys/System/Volumes/Data/opt/homebrew/Cellar/wget");

    let dry = free_report(&fx.free(&list, &["--dry-run", "--json"]));
    assert_eq!(dry.actions[0].result, ActionResult::WouldRun);
    assert_eq!(dry.actions[0].kegs.len(), 1);
    assert!(rack.exists());
    assert!(!fx.calls().iter().any(|c| c.starts_with("uninstall")), "a dry run never uninstalls");

    let out = fx.free(&list, &["--json"]);
    let ran = free_report(&out);
    assert_eq!(ran.actions[0].result, ActionResult::Freed, "{:?}", ran.actions[0]);
    assert!(ran.actions[0].bytes_before.unwrap() >= 40_000);
    assert_eq!(ran.actions[0].bytes_after, Some(0));
    assert!(!rack.exists());
    let calls = fx.calls();
    let tail: Vec<&str> = calls.iter().rev().take(3).rev().map(String::as_str).collect();
    assert_eq!(
        tail,
        [
            "info --json=v2 --installed | 1 1 1 1",
            "services list --json | 1 1 1 1",
            "uninstall --formula wget | 1 1 1 1"
        ],
        "the check right before brew uninstall, every call under the four variables"
    );
    let journal = fs::read_to_string(fx.root.join("data/logs/actions.jsonl")).unwrap();
    assert!(journal.contains(r#""action":"brew-uninstall""#), "{journal}");
}

#[test]
fn a_formula_something_needs_is_refused_with_what_needs_it() {
    let fx = Fx::new();
    let list = json!({"schema_version": 3, "actions": [
        {"action": "brew-uninstall", "identifier": "openssl@3", "kegs": ["3.4.0"]}]})
    .to_string();
    let out = fx.free(&list, &["--json"]);
    let r = free_report(&out);
    assert_eq!(r.actions[0].result, ActionResult::Refused { reason: Refusal::BrewNeeded });
    assert_eq!(r.actions[0].needed_by[0].formulae, ["curl", "wget"]);
    assert!(!fx.calls().iter().any(|c| c.starts_with("uninstall")));

    let text = fx.free(&list, &[]);
    let stdout = String::from_utf8_lossy(&text.stdout);
    assert!(stdout.contains("Homebrew formula openssl@3  refused: something installed still needs it"), "{stdout}");
    let bad = fx.free(
        r#"{"schema_version": 3, "actions": [{"action": "brew-uninstall", "identifier": "--cask", "kegs": ["1"]}]}"#,
        &["--json"],
    );
    assert_eq!(bad.status.code(), Some(2));
    assert!(fx.calls().iter().all(|c| !c.starts_with("uninstall")));
}
