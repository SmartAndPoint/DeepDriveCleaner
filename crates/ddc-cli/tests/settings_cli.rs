//! `settings.json` end to end with a temporary HOME and data directory (story
//! "CLI: shared settings, types and the notify decision"). Every clean here
//! deletes `safe` items only; nothing goes to the real Trash.

use ddc_loadgen::isolate::ClearEnv as _;
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

mod common;

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    data: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let fx = Self { home: root.join("home"), data: root.join("data"), root, _tmp: tmp };
        for rel in ["p/__pycache__/a.pyc", "p/x/__pycache__/b.pyc", "q/__pycache__/c.pyc"] {
            fx.write(rel, 10_000);
        }
        fx
    }

    fn write(&self, rel: &str, len: usize) {
        let p = self.home.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![7u8; len]).unwrap();
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.home.join(rel)
    }

    fn arg(&self, rel: &str) -> String {
        self.path(rel).display().to_string()
    }

    fn settings(&self, json: serde_json::Value) {
        fs::create_dir_all(&self.data).unwrap();
        fs::write(self.data.join("settings.json"), json.to_string()).unwrap();
    }

    fn ddc(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"));
        cmd.clear_env()
            .home_env(&self.home)
            .env("PATH", "")
            .env("NO_COLOR", "1")
            .args(args)
            .arg("--data-dir")
            .arg(&self.data)
            .stdin(Stdio::null());
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.ddc(args).output().unwrap()
    }

    fn scan(&self, args: &[&str]) -> serde_json::Value {
        let out = self.run(&[&["scan", "--json"], args].concat());
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    }

    /// `ddc` on a pseudo-terminal answering `answer`; returns what it printed.
    fn in_terminal(&self, args: &[&str], answer: &str) -> String {
        let ddc = self.ddc(args);
        let mut script = common::script(ddc.get_program(), &ddc.get_args().collect::<Vec<_>>()).expect("a terminal");
        script.clear_env();
        for (k, v) in ddc.get_envs() {
            script.env(k, v.unwrap());
        }
        let mut child = script.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(answer.as_bytes()).unwrap();
        let started = Instant::now();
        while child.try_wait().unwrap().is_none() {
            assert!(started.elapsed() < Duration::from_secs(60), "ddc did not finish");
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(stdin);
        let mut out = String::new();
        child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
        out
    }
}

fn paths(v: &serde_json::Value, key: &str) -> Vec<String> {
    v[key].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_owned()).collect()
}

fn finding<'a>(r: &'a serde_json::Value, path: &str) -> &'a serde_json::Value {
    r["findings"].as_array().unwrap().iter().find(|f| f["path"] == path).unwrap_or_else(|| panic!("no {path}"))
}

#[test]
fn the_settings_apply_and_a_flag_wins() {
    if common::unsupported("the_settings_apply_and_a_flag_wins") {
        return;
    }
    let fx = Fx::new();
    fx.settings(serde_json::json!({
        "schema_version": 1,
        "roots": [fx.path("p"), fx.path("unplugged")],
        "excludes": [fx.path("p/x")],
        "min_age_days": 30,
    }));

    let out = fx.run(&["scan", "--json"]);
    assert!(String::from_utf8_lossy(&out.stderr).contains("unplugged in settings.json does not exist; skipped"));
    let r: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(paths(&r, "scan_roots"), [fx.arg("p")], "the settings' roots, the missing one skipped");
    assert_eq!(r["min_age_days"], 30);
    assert_eq!(finding(&r, &fx.arg("p/x"))["rule"], "user.kept", "the settings' exclude");

    let flagged = fx.scan(&[&fx.arg("q"), "--min-age", "3"]);
    assert_eq!(paths(&flagged, "scan_roots"), [fx.arg("q")]);
    assert_eq!(flagged["min_age_days"], 3);

    let background = fx.run(&["scan", "--background", "--no-notify"]);
    assert!(background.status.success() && background.stderr.is_empty(), "silent");
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(fx.data.join("last-scan.json")).unwrap()).unwrap();
    assert_eq!(paths(&saved, "scan_roots"), [fx.arg("p")], "the background scan reads the settings too");
    assert_eq!(saved["min_age_days"], 30);
    let log = fs::read_to_string(fx.data.join("logs/background.log")).unwrap();
    assert!(log.contains("unplugged in settings.json does not exist; skipped"), "{log}");
}

#[test]
fn switched_off_types_and_small_items_are_ignored() {
    if common::unsupported("switched_off_types_and_small_items_are_ignored") {
        return;
    }
    let fx = Fx::new();
    fx.write("big/__pycache__/d.pyc", 200_000);
    fx.write(".npm/_cacache/content/blob", 30_000);
    fx.settings(serde_json::json!({ "schema_version": 1, "disabled_rules": ["cache.npm"], "min_size": 100_000 }));

    let r = fx.scan(&[]);
    let npm = finding(&r, &fx.arg(".npm/_cacache"));
    assert_eq!((&npm["ignored"], &npm["selected"]), (&serde_json::json!(true), &serde_json::json!(false)));
    assert_eq!(finding(&r, &fx.arg("p/__pycache__"))["ignored"], true, "under min_size");
    assert!(finding(&r, &fx.arg("big/__pycache__")).get("ignored").is_none(), "absent when false");
    assert_eq!(r["totals"]["ignored_count"], 4);

    let text = String::from_utf8(fx.run(&["scan"]).stdout).unwrap();
    assert!(text.contains("4 items ignored by settings.json"), "{text}");
    assert!(!text.contains("~/q/__pycache__"), "ignored items are not listed: {text}");
    let all = String::from_utf8(fx.run(&["scan", "--all"]).stdout).unwrap();
    let line = all.lines().find(|l| l.contains("~/q/__pycache__")).unwrap_or_else(|| panic!("{all}"));
    assert!(line.contains("python.pycache") && line.trim_end().ends_with("ignored"), "marked: {line}");
}

#[test]
fn a_malformed_settings_file_or_no_existing_root_is_an_error_naming_it() {
    if common::unsupported("a_malformed_settings_file_or_no_existing_root_is_an_error_naming_it") {
        return;
    }
    let fx = Fx::new();
    fs::create_dir_all(&fx.data).unwrap();
    let named = fx.data.join("settings.json").display().to_string();
    let unplugged = serde_json::json!({ "schema_version": 1, "roots": [fx.path("unplugged")] }).to_string();
    for (settings, error) in [
        (r#"{"schema_version": 1, "roots": "~"}"#.to_owned(), "malformed settings"),
        (unplugged, "none of the folders to scan"),
    ] {
        fs::write(fx.data.join("settings.json"), settings).unwrap();
        for args in [
            &["scan"][..],
            &["clean", "--dry-run"],
            &["scan", "--background", "--no-notify"],
            &["scan", "--background", "--notify", "json"],
        ] {
            let out = fx.run(args);
            assert_eq!(out.status.code(), Some(1), "{args:?}");
            assert!(out.stdout.is_empty(), "{args:?}: no report and no decision");
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(stderr.contains(&named) && stderr.contains(error), "{args:?}: {stderr}");
        }
    }
}

#[test]
fn an_exclude_flag_that_drops_a_protected_folder_names_it_first() {
    if common::unsupported("an_exclude_flag_that_drops_a_protected_folder_names_it_first") {
        return;
    }
    let fx = Fx::new();
    fx.settings(serde_json::json!({ "schema_version": 1, "excludes": [fx.path("p/x")] }));
    let q = fx.arg("q");
    let named = "no longer protected: ~/p/x";

    for args in [&["scan", "--exclude", &q][..], &["clean", "--dry-run", "--exclude", &q]] {
        let out = fx.run(args);
        assert!(out.status.success(), "{args:?}: changes nothing, so it only warns");
        assert!(String::from_utf8_lossy(&out.stderr).contains(named), "{args:?}");
    }
    // Moved aside after each scan, so a refusal can be seen to write none.
    let set_aside = |n: &str| fs::rename(fx.data.join("last-scan.json"), fx.root.join(n)).unwrap();
    set_aside("scan-1.json");
    let above = fx.run(&["scan", "--exclude", &fx.arg("p")]);
    assert!(!String::from_utf8_lossy(&above.stderr).contains("no longer protected"), "an exclude above still keeps it");
    set_aside("scan-2.json");

    let list = fx.root.join("list.json");
    let item = serde_json::json!({
        "path": fx.path("q/__pycache__"), "dev": 1, "ino": 1, "rule": "python.pycache", "class": "safe", "action": "delete",
    });
    fs::write(&list, serde_json::json!({ "schema_version": 1, "items": [item] }).to_string()).unwrap();
    for args in [
        &["clean", "--exclude", &q][..],
        &["clean", "--yes", "--exclude", &q],
        &["clean", "--items", list.to_str().unwrap(), "--exclude", &q],
        &["clean", "--items", list.to_str().unwrap(), "--dry-run", "--exclude", &q],
    ] {
        let out = fx.run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: nobody to ask");
        assert!(String::from_utf8_lossy(&out.stderr).contains(named), "{args:?}");
    }
    assert!(!fx.data.join("last-scan.json").exists(), "refused before the scan");

    let declined = fx.in_terminal(&["clean", "--yes", "--exclude", &q], "n\n");
    assert!(declined.contains("these would no longer be protected") && declined.contains("~/p/x"), "{declined}");
    assert!(declined.contains("Nothing was changed."), "{declined}");
    assert!(fx.path("p/x/__pycache__").exists() && fx.path("p/__pycache__").exists());
    assert!(!fx.data.join("last-scan.json").exists(), "asked before the lock and the scan");

    fx.in_terminal(&["clean", "--yes", "--exclude", &q], "y\n");
    assert!(!fx.path("p/x/__pycache__").exists(), "confirmed: no longer protected");
    assert!(fx.path("q/__pycache__").exists(), "the flag's exclude");
}

#[test]
fn the_min_size_setting_is_a_floor_under_the_min_size_flag() {
    if common::unsupported("the_min_size_setting_is_a_floor_under_the_min_size_flag") {
        return;
    }
    let fx = Fx::new();
    for (rel, mib) in [("s30/__pycache__/a.pyc", 30), ("s70/__pycache__/a.pyc", 70), ("s110/__pycache__/a.pyc", 110)] {
        fx.write(rel, mib << 20);
    }
    fx.settings(serde_json::json!({ "schema_version": 1, "min_size": 50u64 << 20 }));
    // Every other finding of the fixture is far under both floors. The group
    // line counts planned items, selected or not: none under the floor is planned.
    let planned = |min_size: &str| -> (String, String) {
        let out = fx.run(&["clean", "--dry-run", "--min-size", min_size]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8(out.stdout).unwrap();
        let line =
            |needle: &str| text.lines().find(|l| l.contains(needle)).unwrap_or_else(|| panic!("{text}")).to_owned();
        (line("Cleanup plan:"), line("python.pycache"))
    };

    let (head, group) = planned("10M");
    assert!(
        head.contains("2 items selected, 180.0 MiB") && group.contains(" 2/2 "),
        "the setting's 50 MiB wins: {group}"
    );
    let (head, group) = planned("100M");
    assert!(head.contains("1 items selected, 110.0 MiB") && group.contains(" 1/1 "), "a larger flag wins: {group}");
}

/// "Skip entirely": the scan lists a skip as not scanned, the terminal says so
/// in one line, and the background scan applies it beside the prompting areas.
#[test]
fn a_skip_is_not_scanned_in_every_scan() {
    if common::unsupported("a_skip_is_not_scanned_in_every_scan") {
        return;
    }
    let fx = Fx::new();
    fx.settings(serde_json::json!({ "schema_version": 1, "skips": [fx.path("p/x")] }));

    let r = fx.scan(&[]);
    assert_eq!(paths(&r, "not_scanned"), [fx.arg("p/x")]);
    assert!(r["findings"].as_array().unwrap().iter().all(|f| !f["path"].as_str().unwrap().starts_with(&fx.arg("p/x"))));

    let flagged = fx.scan(&["--skip", &fx.arg("q")]);
    assert_eq!(paths(&flagged, "not_scanned"), [fx.arg("p/x"), fx.arg("q")], "--skip adds to the file's skips");

    let text = String::from_utf8(fx.run(&["scan"]).stdout).unwrap();
    assert!(text.contains("Not scanned: ~/p/x"), "{text}");

    let out = fx.run(&["scan", "--background", "--notify", "json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(fx.data.join("last-scan.json")).unwrap()).unwrap();
    assert_eq!(paths(&saved, "not_scanned"), [fx.arg("p/x")], "the background scan keeps the skips");

    // An `--exclude` replaces the file's excludes; one a skip still covers is not "dropped".
    fx.settings(serde_json::json!({
        "schema_version": 1, "skips": [fx.path("p/x")], "excludes": [fx.path("p/x/__pycache__")],
    }));
    let replaced = fx.run(&["scan", "--exclude", &fx.arg("q")]);
    assert!(replaced.status.success());
    assert!(!String::from_utf8_lossy(&replaced.stderr).contains("no longer protected"), "covered by the skip");

    let file = fx.run(&["scan", "--skip", &fx.arg("p/__pycache__/a.pyc")]);
    assert!(!file.status.success());
    assert!(String::from_utf8_lossy(&file.stderr).contains("cannot skip"), "{}", String::from_utf8_lossy(&file.stderr));
}
