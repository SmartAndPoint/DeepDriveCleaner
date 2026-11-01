//! `ddc scan` end to end with a temporary HOME and data directory.

use ddc_core::platform::Canonical as _;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use ddc_core::model::ScanReport;
use ddc_loadgen::isolate::ClearEnv as _;

mod common;
mod contract;

fn ddc(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .clear_env()
        .home_env(home)
        .env("PATH", "")
        .env("NO_COLOR", "1")
        .args(args)
        .output()
        .unwrap()
}

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonical().unwrap().join("home");
    let w = |rel: &str, content: &[u8]| {
        let p = home.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    };
    w("p/package.json", b"{}");
    w("p/package-lock.json", b"{}");
    w("p/node_modules/x/index.js", &[1u8; 30_000]);
    w(".npm/_cacache/content/blob", &[2u8; 90_000]);
    w("Pictures/a.jpg", &[3u8; 5_000]);
    (tmp, home)
}

#[test]
fn json_report_and_saved_scan() {
    if common::unsupported("json_report_and_saved_scan") {
        return;
    }
    let (tmp, home) = fixture();
    let data = tmp.path().join("data");

    let out = ddc(&home, &["scan", "--json", "--data-dir", data.to_str().unwrap()]);

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let report = serde_json::to_value(contract::holds::<ScanReport>("scan", &out.stdout)).unwrap();
    let rules: Vec<&str> = report["findings"].as_array().unwrap().iter().map(|f| f["rule"].as_str().unwrap()).collect();
    assert_eq!(rules, ["cache.npm", "node.node_modules", "user.pictures"]);
    assert_eq!(report["schema_version"], 4);
    let f = &report["findings"];
    let classes: Vec<&str> = f.as_array().unwrap().iter().map(|x| x["class"].as_str().unwrap()).collect();
    assert_eq!(classes, ["safe", "rebuildable", "user-data"]);
    let selected: Vec<bool> = f.as_array().unwrap().iter().map(|x| x["selected"].as_bool().unwrap()).collect();
    assert_eq!(selected, [true, false, false]);
    for (i, written) in [(0, 90_000u64), (1, 30_000), (2, 5_000)] {
        let bytes = f[i]["bytes"].as_u64().unwrap();
        assert!(bytes >= written && bytes.is_multiple_of(512), "finding {i}: {bytes} bytes for {written} written");
        assert_eq!(f[i]["shared_bytes"], 0);
    }
    assert_eq!(f[1]["project"]["root"], home.join("p").to_str().unwrap());
    assert_eq!(f[1]["project"]["has_lockfile"], true);
    assert_eq!(f[1]["not_selected"]["reason"], "active-project");
    assert!(f[0].get("project").is_none(), "caches have no project");

    let mut names: Vec<String> =
        fs::read_dir(&data).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, ["last-scan.json", "lock"], "no temp files, nothing outside --data-dir");
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(data.join("last-scan.json")).unwrap()).unwrap();
    assert_eq!(saved["findings"], report["findings"]);
    assert!(!home.join("Library").exists(), "nothing written into HOME");
}

/// The hidden `--no-times` (story "Review window: sort a group's rows by
/// name, size or last change") is only for measuring their cost: it takes
/// every finding's `modified` away and changes nothing else, and a user-data
/// folder's entries keep their own times.
#[test]
fn a_scan_without_times_reports_the_same_findings_without_their_dates() {
    if common::unsupported("a_scan_without_times_reports_the_same_findings_without_their_dates") {
        return;
    }
    let (tmp, home) = fixture();
    let big = home.join("Pictures/big.mov");
    fs::write(&big, vec![4u8; 10 << 20]).unwrap();
    let data = tmp.path().join("data");
    let scan = |extra: &[&str]| {
        let out = ddc(&home, &[&["scan", "--json", "--data-dir", data.to_str().unwrap()], extra].concat());
        assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
        serde_json::to_value(contract::holds::<ScanReport>("scan", &out.stdout)).unwrap()
    };

    let (mut with, without) = (scan(&[]), scan(&["--no-times"]));

    let dated = |r: &serde_json::Value, rule: &str| {
        let f = r["findings"].as_array().unwrap().iter().find(|f| f["rule"] == rule).unwrap();
        f.get("modified").cloned()
    };
    for rule in ["node.node_modules", "cache.npm"] {
        assert!(dated(&with, rule).is_some_and(|m| m.is_string()), "{rule}: {with}");
        assert_eq!(dated(&without, rule), None, "{rule}: absent, not null");
    }
    assert_eq!(dated(&with, "user.pictures"), None, "user data has no date");
    for f in with["findings"].as_array_mut().unwrap() {
        f.as_object_mut().unwrap().remove("modified");
    }
    assert_eq!(with["findings"], without["findings"]);
    let pictures = without["findings"].as_array().unwrap().iter().find(|f| f["rule"] == "user.pictures").unwrap();
    let entry = &pictures["contents"]["entries"][0];
    assert_eq!(entry["path"], big.to_str().unwrap());
    assert!(entry["modified"].is_string(), "{entry}");
    let help = ddc(&home, &["scan", "--help"]);
    assert!(!String::from_utf8_lossy(&help.stdout).contains("--no-times"), "hidden");
}

#[test]
fn human_report_lists_classes_in_order_with_totals() {
    if common::unsupported("human_report_lists_classes_in_order_with_totals") {
        return;
    }
    let (tmp, home) = fixture();
    let data = tmp.path().join("data");

    let out = ddc(&home, &["scan", "--data-dir", data.to_str().unwrap()]);

    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(!text.contains('\x1b'), "NO_COLOR disables colors");
    let pos = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("{needle:?} missing in:\n{text}"));
    assert!(pos("SAFE —") < pos("REBUILDABLE —"));
    assert!(pos("REBUILDABLE —") < pos("USER DATA —"));
    assert!(pos("~/.npm/_cacache") > pos("SAFE —"));
    for class in ["safe", "rebuildable", "review", "user-data"] {
        assert!(text.lines().any(|l| l.trim_start().starts_with(class) && l.contains("items")), "total for {class}");
    }
}

#[test]
fn human_report_orders_findings_by_size() {
    if common::unsupported("human_report_orders_findings_by_size") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonical().unwrap().join("home");
    for (rel, n) in
        [("small/__pycache__/a.pyc", 4_000usize), ("big/__pycache__/a.pyc", 400_000), ("mid/__pycache__/a.pyc", 40_000)]
    {
        let p = home.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![9u8; n]).unwrap();
    }
    let data = tmp.path().join("data");

    let out = ddc(&home, &["scan", "--data-dir", data.to_str().unwrap()]);

    let text = String::from_utf8(out.stdout).unwrap();
    let pos = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("{needle:?} missing in:\n{text}"));
    assert!(pos("~/big/__pycache__") < pos("~/mid/__pycache__"));
    assert!(pos("~/mid/__pycache__") < pos("~/small/__pycache__"));
}

// Unix only: it stops and signals the scan's process group.
#[cfg(unix)]
#[test]
fn sigterm_before_the_scan_finishes_leaves_the_saved_scan_untouched() {
    use std::os::unix::process::CommandExt as _;
    use std::process::Stdio;

    use ddc_core::platform::signals::kill_group;

    if common::unsupported("sigterm_before_the_scan_finishes_leaves_the_saved_scan_untouched") {
        return;
    }
    let (tmp, home) = fixture();
    let data = tmp.path().join("data");
    fs::create_dir_all(&data).unwrap();
    let saved = br#"{"schema_version":1,"seeded":true}"#;
    fs::write(data.join("last-scan.json"), saved).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .clear_env()
        .home_env(&home)
        .env("PATH", "")
        .args(["scan", "--json", "--data-dir", data.to_str().unwrap()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap();
    // Stopped at once, so the walk cannot finish before the SIGTERM arrives.
    let group = i32::try_from(child.id()).unwrap();
    kill_group(group, libc::SIGSTOP);
    kill_group(group, libc::SIGTERM);
    kill_group(group, libc::SIGCONT);
    let status = child.wait().unwrap();

    assert_eq!(
        ddc_loadgen::platform::exit_signal(&status),
        Some(libc::SIGTERM),
        "a scan has no handler: the default action ends it"
    );
    assert_eq!(fs::read(data.join("last-scan.json")).unwrap(), saved);
    let leftovers: Vec<_> = fs::read_dir(&data)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".last-scan.json.tmp-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

// Story "CLI: progress events".

#[test]
fn a_scan_with_progress_json_emits_walk_ticks_ending_at_the_report_totals() {
    if common::unsupported("a_scan_with_progress_json_emits_walk_ticks_ending_at_the_report_totals") {
        return;
    }
    let (tmp, home) = fixture();
    let data = tmp.path().join("data");

    let out = ddc(&home, &["scan", "--json", "--progress", "json", "--data-dir", data.to_str().unwrap()]);

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let report = serde_json::to_value(contract::holds::<ScanReport>("scan", &out.stdout)).unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let mut events = Vec::new();
    for line in stderr.lines().filter(|l| !l.trim().is_empty()) {
        if !line.starts_with('{') {
            assert!(line.starts_with("ddc:"), "{line:?}");
            continue;
        }
        let event: ddc_core::progress::ProgressLine = contract::holds("progress", line.as_bytes());
        events.push(serde_json::to_value(&event).unwrap());
    }
    assert!(!events.is_empty());
    assert!(events.iter().all(|e| e["event"] == "walk" && e["step"] == "scan" && e.get("files_expected").is_none()));
    let last = events.last().unwrap();
    let totals = &report["totals"];
    assert_eq!(
        (&last["files"], &last["dirs"], &last["bytes"]),
        (&totals["files"], &totals["dirs"], &totals["scanned_bytes"])
    );
}

/// The line a scan of other roots prints when it saves into the app's data
/// folder (story "Review window: say when the saved scan is not of the home
/// folder").
const APP_SHOWS: &str = "The app will show this report until its next scan.";

fn shows_line(out: &Output) -> bool {
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stderr).contains(APP_SHOWS)
}

#[test]
fn scan_of_other_roots_into_the_app_data_folder_says_the_app_shows_it() {
    if common::unsupported("scan_of_other_roots_into_the_app_data_folder_says_the_app_shows_it") {
        return;
    }
    let (tmp, home) = fixture();
    let other = tmp.path().canonical().unwrap().join("other");
    fs::create_dir_all(other.join("x")).unwrap();
    let other = other.to_str().unwrap();

    assert!(shows_line(&ddc(&home, &["scan", other])));
    assert!(common::default_data(&home).join("last-scan.json").is_file());
    let inside = home.join("p");
    assert!(shows_line(&ddc(&home, &["scan", inside.to_str().unwrap()])), "a part of the home is other roots too");

    let data = tmp.path().join("data");
    assert!(!shows_line(&ddc(&home, &["scan", other, "--data-dir", data.to_str().unwrap()])));
    let by_var = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .clear_env()
        .home_env(&home)
        .env("PATH", "")
        .env("DDC_DATA_DIR", &data)
        .args(["scan", other])
        .output()
        .unwrap();
    assert!(!shows_line(&by_var));
    assert!(!shows_line(&ddc(&home, &["scan"])), "no paths: the settings' own roots");
}

#[test]
fn scan_of_the_settings_roots_by_path_says_nothing() {
    if common::unsupported("scan_of_the_settings_roots_by_path_says_nothing") {
        return;
    }
    let (tmp, home) = fixture();
    let root = tmp.path().canonical().unwrap();
    let other = root.join("other");
    fs::create_dir_all(&other).unwrap();
    let data = common::default_data(&home);
    fs::create_dir_all(&data).unwrap();
    let roots = serde_json::json!({ "schema_version": 1, "roots": [home, other] });
    fs::write(data.join("settings.json"), roots.to_string()).unwrap();

    assert!(!shows_line(&ddc(&home, &["scan", other.to_str().unwrap(), home.to_str().unwrap()])));
    // Through a path that is not canonical, as the walk normalizes it.
    let dotted = format!("{}/../home", home.display());
    assert!(shows_line(&ddc(&home, &["scan", &dotted])), "one of two settings roots is other roots");
    assert!(!shows_line(&ddc(&home, &["scan", &dotted, other.to_str().unwrap()])));
}
