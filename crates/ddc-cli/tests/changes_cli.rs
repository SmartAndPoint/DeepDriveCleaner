//! `ddc changes` (story "ddc changes: what grew between the last two
//! scans", DDC-001), end to end with a temporary home and data folder. The
//! core's tests (`crates/ddc-core/tests/changes.rs`) cover the rules on
//! hand-built reports; these run the real scans and cleans. No item goes to
//! the real Trash: every item cleaned here is deleted.

use ddc_core::platform::Canonical as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime};

use ddc_core::changes::{ChangesReport, ItemKind, Reason, Status};
use ddc_core::env::Env;
use ddc_core::holders::HolderCommand;
use ddc_core::model::ScanReport;
use ddc_core::store::{self, LockMode};
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
    /// A home with a Rust project `p` and a `__pycache__` in `q`, and a data
    /// folder whose settings turn the low-disk check off.
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonical().unwrap();
        let fx = Self { home: root.join("home"), data: root.join("data"), root, _tmp: tmp };
        fx.write("p/Cargo.toml", 100);
        fx.write("p/target/debug/a.bin", 200_000);
        fx.write("q/__pycache__/m.pyc", 50_000);
        fs::create_dir_all(&fx.data).unwrap();
        fs::write(fx.data.join("settings.json"), r#"{"schema_version": 1, "notify_low_disk": false}"#).unwrap();
        fx
    }

    fn write(&self, rel: &str, n: usize) {
        let p = self.home.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![7u8; n]).unwrap();
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"));
        cmd.clear_env().home_env(&self.home).env("PATH", "").env("NO_COLOR", "1").args(args).stdin(Stdio::null());
        cmd
    }

    /// `ddc args --data-dir <data>`, which must exit 0.
    fn ddc(&self, args: &[&str]) -> Output {
        let out = self.cmd(args).arg("--data-dir").arg(&self.data).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        out
    }

    fn scan(&self) -> ScanReport {
        contract::holds("scan", &self.ddc(&["scan", "--json"]).stdout)
    }

    fn changes(&self) -> ChangesReport {
        contract::holds("changes", &self.ddc(&["changes", "--json"]).stdout)
    }

    /// `ddc clean --items` of the finding at `rel` in `report`, deleted.
    fn clean(&self, report: &ScanReport, rel: &str) {
        let path = self.home.join(rel);
        let f = report.findings.iter().find(|f| f.path == path).unwrap_or_else(|| panic!("no finding at {rel}"));
        let item = serde_json::json!({
            "path": f.path, "dev": f.dev, "ino": f.ino, "rule": f.rule, "class": f.class, "action": "delete",
        });
        let list = self.root.join("list.json");
        fs::write(&list, serde_json::json!({ "schema_version": 1, "items": [item] }).to_string()).unwrap();
        self.ddc(&["clean", "--items", list.to_str().unwrap(), "--json"]);
        assert!(!path.exists(), "{rel} was cleaned");
    }

    /// Every file under the data folder, with its bytes and its mtime.
    fn files(&self) -> Vec<(PathBuf, Vec<u8>, SystemTime)> {
        let mut out = Vec::new();
        let mut stack = vec![self.data.clone()];
        while let Some(dir) = stack.pop() {
            for e in fs::read_dir(&dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    out.push((p.clone(), fs::read(&p).unwrap(), fs::metadata(&p).unwrap().modified().unwrap()));
                }
            }
        }
        out.sort();
        out
    }
}

fn item<'a>(r: &'a ChangesReport, path: &Path) -> &'a ddc_core::changes::Item {
    r.items.iter().find(|i| i.path == path).unwrap_or_else(|| panic!("no row at {path:?}: {:#?}", r.items))
}

/// S1's flow: a clean edits the report it cleaned, so the next scan's
/// baseline no longer holds the item, and a rebuilt `target/` is new.
#[test]
fn a_target_cleaned_through_the_app_then_rebuilt_is_new_and_the_clean_is_not_counted() {
    if common::unsupported("a_target_cleaned_through_the_app_then_rebuilt_is_new_and_the_clean_is_not_counted") {
        return;
    }
    let fx = Fx::new();
    let first = fx.scan();
    assert_eq!(fx.changes().reason, Some(Reason::NoPrevious));
    fx.clean(&first, "p/target");
    fx.write("p/target/debug/a.bin", 400_000);
    fx.scan();
    let r = fx.changes();
    assert_eq!(r.status, Status::Compared, "{:?}", r.reason);
    let target = item(&r, &fx.home.join("p/target"));
    assert_eq!((target.kind, target.before), (ItemKind::New, None));
    let since = r.cleaned_since.unwrap();
    assert_eq!((since.deleted, since.trashed), (0, 0), "the baseline is the edited report");
}

/// A clean between two scans counts as the person's, also with a broken
/// line in the journal; the text says so.
#[test]
fn a_clean_between_two_scans_is_cleaned_by_you_whatever_else_the_journal_holds() {
    if common::unsupported("a_clean_between_two_scans_is_cleaned_by_you_whatever_else_the_journal_holds") {
        return;
    }
    let fx = Fx::new();
    fx.scan();
    let second = fx.scan();
    fx.clean(&second, "q/__pycache__");
    let journal = fx.data.join("logs/actions.jsonl");
    let lines = fs::read_to_string(&journal).unwrap();
    fs::write(&journal, format!("{{ broken\n{lines}")).unwrap();
    let r = fx.changes();
    assert_eq!(r.status, Status::Compared, "{:?}", r.reason);
    let pyc = item(&r, &fx.home.join("q/__pycache__"));
    assert_eq!(pyc.kind, ItemKind::Cleaned);
    assert!(r.cleaned_since.unwrap().deleted > 0);
    let text = String::from_utf8(fx.ddc(&["changes"]).stdout).unwrap();
    assert!(text.contains("~/q/__pycache__  (cleaned by you)"), "{text}");
    assert!(text.contains("You cleaned "), "{text}");
}

#[test]
fn changes_reads_without_the_lock_writes_nothing_and_honours_ddc_data_dir() {
    if common::unsupported("changes_reads_without_the_lock_writes_nothing_and_honours_ddc_data_dir") {
        return;
    }
    let fx = Fx::new();
    fx.scan();
    fx.write("p/target/debug/b.bin", 300_000);
    fx.scan();
    let before = fx.files();
    let env = Env::with_paths(&fx.home, &fx.data, &fx.data.join("logs"), SystemTime::now()).unwrap();
    let lock = store::try_lock(&env, LockMode::Exclusive, HolderCommand::Clean).unwrap();
    let held = fx.files();
    std::thread::sleep(Duration::from_millis(20));
    let r = fx.changes();
    assert_eq!(r.status, Status::Compared, "never 75: {:?}", r.reason);
    assert_eq!(item(&r, &fx.home.join("p/target")).kind, ItemKind::Grew);
    assert_eq!(fx.files(), held, "no file in the data or log folder changed");
    drop(lock);

    // DDC_DATA_DIR, with no --data-dir.
    let out = fx.cmd(&["changes", "--json"]).env("DDC_DATA_DIR", &fx.data).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let by_var: ChangesReport = contract::holds("changes", &out.stdout);
    assert_eq!(by_var.items, r.items);
    assert!(before.iter().any(|(p, ..)| p.ends_with("previous-scan.json")));
}

#[test]
fn an_unreadable_report_exits_1_with_a_ddc_line_and_prints_no_json() {
    if common::unsupported("an_unreadable_report_exits_1_with_a_ddc_line_and_prints_no_json") {
        return;
    }
    let fx = Fx::new();
    fs::create_dir(fx.data.join("last-scan.json")).unwrap();
    let out = fx.cmd(&["changes", "--json", "--data-dir", fx.data.to_str().unwrap()]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty(), "{}", String::from_utf8_lossy(&out.stdout));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.starts_with("ddc: ") && err.contains("last-scan.json"), "{err}");
}

/// `ddc scan` prints the short form of `ddc changes` after its report when
/// its save kept the report it replaced: not after the first scan, after
/// the second, never with `--json`.
#[test]
fn ddc_scan_prints_what_grew_after_its_second_scan_and_never_with_json() {
    if common::unsupported("ddc_scan_prints_what_grew_after_its_second_scan_and_never_with_json") {
        return;
    }
    let fx = Fx::new();
    let text = |out: Output| String::from_utf8(out.stdout).unwrap();
    let first = text(fx.ddc(&["scan"]));
    assert!(!first.contains("ddc changes") && !first.contains("earlier scan"), "{first}");
    fx.write("p/target/debug/b.bin", 300_000);
    let second = text(fx.ddc(&["scan"]));
    let block = &second[second.rfind("\n\n").unwrap()..];
    assert!(block.contains("\n  Build outputs ") && block.contains("1 grew"), "{second}");
    assert!(block.ends_with("\n`ddc changes` lists every row.\n"), "{second}");
    assert!(!block.contains("p/target"), "the block lists no rows: {second}");
    // A third save keeps the second report, and `--json` stays one report.
    let json = fx.ddc(&["scan", "--json"]).stdout;
    let _: ScanReport = contract::holds("scan", &json);
    assert!(!String::from_utf8(json).unwrap().contains("ddc changes"));
}

/// A kept report without S1's fields gives the one-line reason, nothing
/// more.
#[test]
fn ddc_scan_after_a_report_of_an_older_ddc_says_growth_shows_after_the_next_scan() {
    if common::unsupported("ddc_scan_after_a_report_of_an_older_ddc_says_growth_shows_after_the_next_scan") {
        return;
    }
    let fx = Fx::new();
    fx.scan();
    // A report saved before 0.31 has neither field.
    let last = fx.data.join("last-scan.json");
    let mut old: serde_json::Value = serde_json::from_slice(&fs::read(&last).unwrap()).unwrap();
    let fields = old.as_object_mut().unwrap();
    fields.remove("written_at");
    fields.remove("rules_in_force");
    fs::write(&last, serde_json::to_vec(&old).unwrap()).unwrap();
    let out = String::from_utf8(fx.ddc(&["scan"]).stdout).unwrap();
    assert!(fx.data.join("previous-scan.json").exists(), "the older report was kept");
    assert!(out.ends_with("\n\nGrowth shows after the next scan.\n"), "{out}");
}

/// A journal the block cannot read is a warning: the scan still exits 0,
/// with its report and without the block.
#[test]
fn ddc_scan_that_cannot_read_the_journal_warns_and_prints_no_block() {
    if common::unsupported("ddc_scan_that_cannot_read_the_journal_warns_and_prints_no_block") {
        return;
    }
    let fx = Fx::new();
    fx.scan();
    fs::create_dir_all(fx.data.join("logs/actions.jsonl")).unwrap();
    let out = fx.ddc(&["scan"]);
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(stdout.starts_with("DeepDriveCleaner") && !stdout.contains("ddc changes"), "{stdout}");
    assert!(stderr.contains("ddc: warning: cannot read ") && stderr.contains("actions.jsonl"), "{stderr}");
}
