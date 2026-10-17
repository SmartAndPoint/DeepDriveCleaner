//! `ddc scan --background`: scan-only, silent, no TCC areas.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

use ddc_core::env::Env;
use ddc_core::store::{self, LockMode};

fn ddc(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .env_clear()
        .env("HOME", home)
        .env("PATH", "")
        .args(args)
        .output()
        .unwrap()
}

fn tree() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let home = root.join("home");
    for (rel, n) in
        [("p/__pycache__/m.pyc", 5_000usize), (".npm/_cacache/blob", 50_000), ("Documents/node_modules/x.js", 100)]
    {
        let p = home.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![1u8; n]).unwrap();
    }
    (tmp, home, root.join("data"))
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u64, fs::FileType)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
            let md = fs::symlink_metadata(e.path()).unwrap();
            out.insert(e.path(), (md.ino(), md.file_type()));
            if md.is_dir() {
                stack.push(e.path());
            }
        }
    }
    out
}

#[test]
fn background_scan_only_reads_and_saves_the_report() {
    let (_tmp, home, data) = tree();
    let before = snapshot(&home);

    let out = ddc(&home, &["scan", "--background", "--no-notify", "--data-dir", data.to_str().unwrap()]);

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "silent");
    assert_eq!(snapshot(&home), before, "nothing changed in HOME");
    assert!(!data.join("logs/actions.jsonl").exists(), "no cleanup journal");
    let report: serde_json::Value = serde_json::from_slice(&fs::read(data.join("last-scan.json")).unwrap()).unwrap();
    let rules: Vec<&str> = report["findings"].as_array().unwrap().iter().map(|f| f["rule"].as_str().unwrap()).collect();
    assert_eq!(rules, ["cache.npm", "python.pycache"], "Documents is skipped, not read");
    assert!(
        report["skipped"].as_array().unwrap().iter().any(|s| s["path"] == home.join("Documents").to_str().unwrap())
    );
    let log = fs::read_to_string(data.join("logs/background.log")).unwrap();
    assert!(log.contains("scanned") && log.contains("notified false"), "{log}");
}

#[test]
fn tcc_areas_are_never_opened() {
    let (_tmp, home, data) = tree();
    // An unreadable Documents would be an access error if the scan opened it.
    fs::set_permissions(home.join("Documents"), fs::Permissions::from_mode(0o000)).unwrap();

    let out = ddc(&home, &["scan", "--background", "--no-notify", "--data-dir", data.to_str().unwrap()]);

    fs::set_permissions(home.join("Documents"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(out.status.success());
    let report: serde_json::Value = serde_json::from_slice(&fs::read(data.join("last-scan.json")).unwrap()).unwrap();
    assert_eq!(report["errors_total"], 0, "{}", report["inaccessible"]);
}

#[test]
fn with_full_disk_access_prompting_areas_are_scanned() {
    let (_tmp, home, data) = tree();
    // Readable TCC.db is how the scan tells it has Full Disk Access.
    let tcc = home.join("Library/Application Support/com.apple.TCC/TCC.db");
    fs::create_dir_all(tcc.parent().unwrap()).unwrap();
    fs::write(&tcc, b"").unwrap();

    let out = ddc(&home, &["scan", "--background", "--no-notify", "--data-dir", data.to_str().unwrap()]);

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let report: serde_json::Value = serde_json::from_slice(&fs::read(data.join("last-scan.json")).unwrap()).unwrap();
    let documents = home.join("Documents");
    assert!(report["skipped"].as_array().unwrap().iter().all(|s| s["path"] != documents.to_str().unwrap()));
    assert!(
        report["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["path"] == documents.join("node_modules").to_str().unwrap()),
        "{}",
        report["findings"]
    );
}

#[test]
fn background_scan_steps_aside_while_the_lock_is_held() {
    let (_tmp, home, data) = tree();
    let env = Env::with_paths(&home, &data, &data.join("logs"), SystemTime::now()).unwrap();
    let _clean = store::try_lock(&env, LockMode::Exclusive).unwrap();

    let out = ddc(&home, &["scan", "--background", "--no-notify", "--data-dir", data.to_str().unwrap()]);

    assert!(out.status.success());
    assert!(!data.join("last-scan.json").exists(), "no scan while cleaning");
    assert!(fs::read_to_string(data.join("logs/background.log")).unwrap().contains("skipped"));
}
