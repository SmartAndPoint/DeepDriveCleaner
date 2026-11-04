//! `previous-scan.json` and the save's stamp (story "Keep the previous scan
//! report, and say who wrote each", DDC-001), end to end with a temporary
//! home and data folder. The store's own tests (`store.rs`) cover every
//! case of the rule; these check that each command saves with its own
//! writer and its default roots. No item goes to the real Trash: the only
//! item cleaned is a `__pycache__`, which is deleted.

use ddc_core::platform::Canonical as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use ddc_core::exec::report::CleanReport;
use ddc_core::model::{ScanReport, WrittenBy};
use ddc_core::rules::listing::RuleList;
use ddc_core::store::{LAST_SCAN, PREVIOUS_SCAN};
use ddc_loadgen::isolate::ClearEnv as _;

mod common;
mod contract;

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    data: PathBuf,
}

impl Fx {
    /// A home with a project `p` and two `__pycache__` folders, and a data
    /// folder whose settings turn the low-disk check off.
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonical().unwrap();
        let fx = Self { home: root.join("home"), data: root.join("data"), root, _tmp: tmp };
        for (rel, n) in [("p/main.py", 100usize), ("q/__pycache__/m.pyc", 10_000), ("r/__pycache__/m.pyc", 5_000)] {
            let p = fx.home.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, vec![1u8; n]).unwrap();
        }
        fs::create_dir_all(&fx.data).unwrap();
        fs::write(fx.data.join("settings.json"), r#"{"schema_version": 1, "notify_low_disk": false}"#).unwrap();
        fx
    }

    /// `ddc args --data-dir <data>`, which must exit 0.
    fn ddc(&self, args: &[&str]) -> Output {
        let data = ["--data-dir", self.data.to_str().unwrap()];
        self.run(&[args, &data].concat())
    }

    /// `ddc args`, which must exit 0.
    fn run(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"));
        cmd.clear_env().home_env(&self.home).env("PATH", "").env("NO_COLOR", "1");
        let out = cmd.args(args).stdin(Stdio::null()).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        out
    }

    /// `ddc scan --json [path]`: the report it printed, which is the saved one.
    fn scan(&self, path: Option<&str>) -> ScanReport {
        let mut args = vec!["scan", "--json"];
        let given = path.map(|rel| self.home.join(rel));
        args.extend(given.as_deref().map(|p| p.to_str().unwrap()));
        let out = self.ddc(&args);
        assert_eq!(out.stdout, [self.last_bytes(), b"\n".to_vec()].concat(), "--json prints what was saved");
        contract::holds("scan", &out.stdout)
    }

    fn last_bytes(&self) -> Vec<u8> {
        fs::read(self.data.join(LAST_SCAN)).unwrap()
    }

    fn last(&self) -> ScanReport {
        contract::holds("scan", &self.last_bytes())
    }

    fn previous(&self) -> Option<Vec<u8>> {
        fs::read(self.data.join(PREVIOUS_SCAN)).ok()
    }

    /// The temp files left in the data folder.
    fn temps(&self) -> Vec<String> {
        fs::read_dir(&self.data)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp-"))
            .collect()
    }

    /// A list of the finding at `rel` in `report`, as the app sends it.
    fn list(&self, report: &ScanReport, rel: &str) -> PathBuf {
        let path = self.home.join(rel);
        let f = report.findings.iter().find(|f| f.path == path).unwrap_or_else(|| panic!("no finding at {rel}"));
        let item = serde_json::json!({
            "path": f.path, "dev": f.dev, "ino": f.ino, "rule": f.rule, "class": f.class, "action": "delete",
        });
        let list = self.root.join("list.json");
        fs::write(&list, serde_json::json!({ "schema_version": 1, "items": [item] }).to_string()).unwrap();
        list
    }

    /// `ddc clean --items <list> --json`; the saved report afterwards.
    fn clean_items(&self, list: &Path) -> ScanReport {
        let out = self.ddc(&["clean", "--items", list.to_str().unwrap(), "--json"]);
        let clean: CleanReport = contract::holds("clean", &out.stdout);
        assert!(clean.last_scan_saved, "{:?}", clean.items);
        self.last()
    }
}

/// RFC 3339 in UTC, to the second.
fn assert_to_the_second(at: Option<&str>) {
    let at = at.expect("a written_at");
    assert!(at.len() == "2026-11-01T10:00:05Z".len() && at.ends_with('Z'), "{at}");
    humantime::parse_rfc3339(at).unwrap();
}

#[test]
fn two_home_scans_then_two_project_scans_keep_the_second_home_scan() {
    if common::unsupported("two_home_scans_then_two_project_scans_keep_the_second_home_scan") {
        return;
    }
    let fx = Fx::new();
    let a = fx.scan(None);
    assert_eq!(a.written_by, Some(WrittenBy::Scan));
    assert_to_the_second(a.written_at.as_deref());
    assert_eq!(a.previous_written_at, None, "nothing before the first scan");
    assert_eq!(fx.previous(), None);

    let b = fx.scan(None);
    let b_bytes = fx.last_bytes();
    assert_eq!(b.previous_written_at, a.written_at);

    // The first project scan keeps the home report; the second finds a
    // project report and keeps nothing.
    let first = fx.scan(Some("p"));
    assert_eq!(fx.previous(), Some(b_bytes.clone()));
    assert_eq!(first.previous_written_at, b.written_at);
    let second = fx.scan(Some("p"));
    assert_eq!(second.scan_roots, [fx.home.join("p")]);
    assert_eq!(fx.previous(), Some(b_bytes), "previous-scan.json is the second home scan");
    assert_eq!(second.previous_written_at, b.written_at);
    assert!(fx.temps().is_empty(), "{:?}", fx.temps());

    // The rules in force are the ids `ddc rules --json` lists, sorted.
    let out = fx.run(&["rules", "--json"]);
    let rules: RuleList = contract::holds("rules", &out.stdout);
    let mut ids: Vec<String> = rules.rules.into_iter().map(|r| r.id).collect();
    ids.sort();
    assert_eq!(a.rules_in_force.as_ref(), Some(&ids));
    assert_eq!(second.rules_in_force.as_ref(), Some(&ids));
}

#[test]
fn a_folder_in_the_place_of_previous_scan_json_is_a_warning_and_the_report_is_saved() {
    if common::unsupported("a_folder_in_the_place_of_previous_scan_json_is_a_warning_and_the_report_is_saved") {
        return;
    }
    let fx = Fx::new();
    let a = fx.scan(None);
    fx.scan(None);
    fs::rename(fx.data.join(PREVIOUS_SCAN), fx.root.join("aside.json")).unwrap();
    fs::create_dir(fx.data.join(PREVIOUS_SCAN)).unwrap();

    let out = fx.ddc(&["scan", "--json"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let warning = stderr.lines().find(|l| l.starts_with("ddc: warning: cannot keep the previous report in "));
    assert!(warning.is_some_and(|l| l.contains(PREVIOUS_SCAN)), "{stderr}");
    let c: ScanReport = contract::holds("scan", &out.stdout);
    assert_eq!(fx.last(), c, "saved all the same");
    assert_eq!(c.previous_written_at, a.written_at, "carried forward from the report it replaced");
    assert!(fx.data.join(PREVIOUS_SCAN).is_dir());
    assert!(fx.temps().is_empty(), "{:?}", fx.temps());

    // The scheduled scan says it in its log.
    fx.ddc(&["scan", "--background", "--no-notify"]);
    let log = fs::read_to_string(fx.data.join("logs/background.log")).unwrap();
    assert!(log.contains("cannot keep the previous report in "), "{log}");
    assert_eq!(fx.last().written_by, Some(WrittenBy::Background));
    assert!(fx.temps().is_empty(), "{:?}", fx.temps());
}

#[test]
fn the_background_scan_and_a_cleans_recheck_keep_the_report_before_them() {
    if common::unsupported("the_background_scan_and_a_cleans_recheck_keep_the_report_before_them") {
        return;
    }
    let fx = Fx::new();
    fx.scan(None);
    let scanned = fx.last_bytes();
    let a = fx.last();

    fx.ddc(&["scan", "--background", "--no-notify"]);
    let background = fx.last();
    assert_eq!(background.written_by, Some(WrittenBy::Background));
    assert_to_the_second(background.written_at.as_deref());
    assert_eq!(background.previous_written_at, a.written_at);
    assert_eq!(fx.previous(), Some(scanned));
    let background_bytes = fx.last_bytes();

    let out = fx.ddc(&["clean", "--dry-run"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("Dry run: nothing was changed."));
    let recheck = fx.last();
    assert_eq!(recheck.written_by, Some(WrittenBy::CleanRecheck));
    assert_to_the_second(recheck.written_at.as_deref());
    assert_eq!(recheck.previous_written_at, background.written_at);
    assert_eq!(fx.previous(), Some(background_bytes));
    assert!(fx.home.join("q/__pycache__").exists(), "a dry run removes nothing");
    assert!(fx.temps().is_empty(), "{:?}", fx.temps());
}

#[test]
fn a_clean_of_a_list_keeps_nothing_and_carries_the_pair_forward() {
    if common::unsupported("a_clean_of_a_list_keeps_nothing_and_carries_the_pair_forward") {
        return;
    }
    let fx = Fx::new();
    let a = fx.scan(None);
    let a_bytes = fx.last_bytes();
    let b = fx.scan(None);

    // The pruned walk edits the report it read.
    let pruned = fx.clean_items(&fx.list(&b, "q/__pycache__"));
    assert!(!fx.home.join("q/__pycache__").exists());
    assert_eq!(pruned.written_by, Some(WrittenBy::CleanItems));
    assert_to_the_second(pruned.written_at.as_deref());
    assert_eq!(pruned.generated_at, b.generated_at, "an edit keeps the scan's start");
    assert_eq!(pruned.rules_in_force, b.rules_in_force, "and its rules");
    assert_eq!(pruned.previous_written_at, a.written_at);
    assert_eq!(fx.previous(), Some(a_bytes), "the report before the clean stays the baseline");

    // After a project scan there is no home report to edit: the full walk.
    fx.scan(None);
    let home = fx.last_bytes();
    let c = fx.last();
    fx.scan(Some("p"));
    let full = fx.clean_items(&fx.list(&c, "r/__pycache__"));
    assert!(!fx.home.join("r/__pycache__").exists());
    assert_eq!(full.scan_roots, std::slice::from_ref(&fx.home), "the whole home was walked");
    assert_eq!(full.written_by, Some(WrittenBy::CleanItems));
    assert_eq!(full.previous_written_at, c.written_at, "carried forward from the project report");
    assert_eq!(fx.previous(), Some(home));
    assert!(fx.temps().is_empty(), "{:?}", fx.temps());
}

#[test]
fn ddc_data_dir_holds_previous_scan_json() {
    if common::unsupported("ddc_data_dir_holds_previous_scan_json") {
        return;
    }
    let fx = Fx::new();
    let elsewhere = fx.root.join("elsewhere");
    for _ in 0..2 {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"));
        cmd.clear_env().home_env(&fx.home).env("PATH", "").env("DDC_DATA_DIR", &elsewhere).args(["scan", "--json"]);
        let out = cmd.output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    }
    let last: ScanReport = contract::holds("scan", &fs::read(elsewhere.join(LAST_SCAN)).unwrap());
    let previous: ScanReport = contract::holds("scan", &fs::read(elsewhere.join(PREVIOUS_SCAN)).unwrap());
    assert_eq!(last.previous_written_at, previous.written_at);
    assert!(!fx.data.join(LAST_SCAN).exists(), "nothing in the --data-dir of the other tests");
}
