//! `ddc scan` end to end with a temporary HOME and data directory.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn ddc(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .env_clear()
        .env("HOME", home)
        .env("PATH", "")
        .env("NO_COLOR", "1")
        .args(args)
        .output()
        .unwrap()
}

fn fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap().join("home");
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
    let (tmp, home) = fixture();
    let data = tmp.path().join("data");

    let out = ddc(&home, &["scan", "--json", "--data-dir", data.to_str().unwrap()]);

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let rules: Vec<&str> = report["findings"].as_array().unwrap().iter().map(|f| f["rule"].as_str().unwrap()).collect();
    assert_eq!(rules, ["cache.npm", "node.node_modules", "user.pictures"]);
    assert_eq!(report["schema_version"], 1);
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

#[test]
fn human_report_lists_classes_in_order_with_totals() {
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
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap().join("home");
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
