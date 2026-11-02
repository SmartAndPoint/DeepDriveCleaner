//! `ddc space` (story "Popover: say when used space grew since the last
//! scan"): against a real scan of a small temporary home, through the real
//! `statfs`. The low-disk check is off in these tests, since the disk they
//! run on may itself be almost full.

use ddc_core::platform::Canonical as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

use ddc_core::env::Env;
use ddc_core::model::ScanReport;
use ddc_core::space::{BaselineSource, SpaceReport, SpaceState};
use ddc_core::store::{self, LockMode};
use ddc_loadgen::isolate::ClearEnv as _;

mod common;
mod contract;

fn ddc(home: &Path, args: &[&str], data_var: Option<&Path>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"));
    cmd.clear_env().home_env(home).env("PATH", "").args(args);
    if let Some(data) = data_var {
        cmd.env("DDC_DATA_DIR", data);
    }
    cmd.output().unwrap()
}

/// A home with one finding, and a data folder whose settings turn the
/// low-disk check off.
fn tree() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonical().unwrap();
    let home = root.join("home");
    let pyc = home.join("p/__pycache__/m.pyc");
    fs::create_dir_all(pyc.parent().unwrap()).unwrap();
    fs::write(&pyc, vec![1u8; 5_000]).unwrap();
    let data = root.join("data");
    fs::create_dir_all(&data).unwrap();
    fs::write(data.join("settings.json"), r#"{"schema_version": 1, "notify_low_disk": false}"#).unwrap();
    (tmp, home, data)
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> =
        fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn space_json_compares_with_the_scan_and_writes_only_its_samples() {
    if common::unsupported("space_json_compares_with_the_scan_and_writes_only_its_samples") {
        return;
    }
    let (_tmp, home, data) = tree();
    let dir = data.to_str().unwrap();
    let scan = ddc(&home, &["scan", "--data-dir", dir], None);
    assert!(scan.status.success(), "{}", stderr(&scan));
    let saved_bytes = fs::read(data.join("last-scan.json")).unwrap();
    let saved_mtime = fs::metadata(data.join("last-scan.json")).unwrap().modified().unwrap();
    let saved: ScanReport = serde_json::from_slice(&saved_bytes).unwrap();
    let before = names(&data);

    let out = ddc(&home, &["space", "--json", "--data-dir", dir], None);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert!(out.stderr.is_empty(), "{}", stderr(&out));
    let r: SpaceReport = contract::holds("space", &out.stdout);
    let volume = saved.volumes.iter().find(|v| v.mount_point == r.volume.mount_point).expect("the home's volume");
    let baseline = r.baseline.as_ref().expect("a baseline");
    assert_eq!(baseline.source, BaselineSource::Scan);
    assert_eq!(baseline.at, saved.generated_at);
    assert_eq!(baseline.free_bytes, volume.free_bytes);
    assert_eq!(r.volume.total_bytes, volume.total_bytes);
    assert!(r.in_use_delta.is_some());
    // Nothing near 20 GiB was written between the two runs.
    assert_eq!((r.state, r.grew), (SpaceState::Quiet, false));

    let mut after = before.clone();
    after.push("space.json".to_owned());
    after.sort();
    assert_eq!(names(&data), after, "space.json is all it adds");
    assert_eq!(fs::read(data.join("last-scan.json")).unwrap(), saved_bytes);
    assert_eq!(fs::metadata(data.join("last-scan.json")).unwrap().modified().unwrap(), saved_mtime);
    let samples: serde_json::Value = serde_json::from_slice(&fs::read(data.join("space.json")).unwrap()).unwrap();
    assert_eq!(samples["samples"][0]["at"], r.checked_at.as_str());
    assert_eq!(samples["samples"][0]["free"], r.volume.free_bytes);
}

#[test]
fn space_says_one_sentence_and_reads_ddc_data_dir() {
    if common::unsupported("space_says_one_sentence_and_reads_ddc_data_dir") {
        return;
    }
    let (_tmp, home, data) = tree();
    let out = ddc(&home, &["space"], Some(&data));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "No saved scan to compare with yet: run `ddc scan`.\n");
    assert!(data.join("space.json").is_file(), "DDC_DATA_DIR is where it keeps its samples");

    let scan = ddc(&home, &["scan"], Some(&data));
    assert!(scan.status.success(), "{}", stderr(&scan));
    let out = ddc(&home, &["space"], Some(&data));
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.starts_with("No more in use on ") && text.ends_with(" than at the last scan.\n"), "{text}");
    assert_eq!(text.lines().count(), 1);
}

#[test]
fn space_runs_while_a_clean_holds_the_lock() {
    if common::unsupported("space_runs_while_a_clean_holds_the_lock") {
        return;
    }
    let (_tmp, home, data) = tree();
    let env = Env::with_paths(&home, &data, &data.join("logs"), SystemTime::now()).unwrap();
    let _clean = store::try_lock(&env, LockMode::Exclusive).unwrap();
    let out = ddc(&home, &["space", "--json", "--data-dir", data.to_str().unwrap()], None);
    assert_eq!(out.status.code(), Some(0), "never 75: {}", stderr(&out));
    let r: SpaceReport = contract::holds("space", &out.stdout);
    assert_eq!((r.baseline, r.in_use_delta, r.grew), (None, None, false));
}

#[test]
fn a_malformed_settings_file_is_a_warning_and_space_still_decides() {
    if common::unsupported("a_malformed_settings_file_is_a_warning_and_space_still_decides") {
        return;
    }
    let (_tmp, home, data) = tree();
    fs::write(data.join("settings.json"), "{ not json").unwrap();
    let out = ddc(&home, &["space", "--json", "--data-dir", data.to_str().unwrap()], None);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    let err = stderr(&out);
    assert!(
        err.starts_with("ddc: warning: malformed settings ") && err.contains("the low-disk check stays on"),
        "{err}"
    );
    let r: SpaceReport = contract::holds("space", &out.stdout);
    // The low-disk check counts again: whichever state the real disk gives, it is one of the rule's.
    let low = r.volume.free_bytes.saturating_mul(10) < r.volume.total_bytes;
    assert_eq!(r.state == SpaceState::LowDisk, low);
}
