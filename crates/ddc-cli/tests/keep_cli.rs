//! `ddc keep remove` end to end (story "Protected by you: say what it is,
//! and Remove Protection", ACs 2 to 5): a temporary HOME and data directory,
//! a `last-scan.json` the test writes, and the lock held by the test. The
//! only file removed is a `.ddc-keep` in the temporary home.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::SystemTime;

use ddc_core::env::Env;
use ddc_core::keep::{KEEP_SCHEMA_VERSION, KeepRefusal, KeepReport, KeepResult};
use ddc_core::model::ScanReport;
use ddc_core::store::{self, LockMode};
use ddc_loadgen::isolate::ClearEnv as _;
use serde_json::Value;

mod common;
mod contract;

/// The rule the test's `last-scan.json` names, so a line that carries it
/// read it from there.
const REPORTED_RULE: &str = "test.from-the-saved-report";

struct Fx {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    data: PathBuf,
}

impl Fx {
    /// Node projects `a` and `b`, each `node_modules` kept by a `.ddc-keep`
    /// file at its top.
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let fx = Self { home: root.join("home"), data: root.join("data"), _tmp: tmp };
        for p in ["a", "b"] {
            fx.write(&format!("{p}/package.json"), b"{}");
            fx.write(&format!("{p}/package-lock.json"), b"{}");
            fx.write(&format!("{p}/node_modules/dep/index.js"), &[7u8; 10_000]);
            fx.write(&format!("{p}/node_modules/.ddc-keep"), b"");
        }
        fx
    }

    fn write(&self, rel: &str, bytes: &[u8]) {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, bytes).unwrap();
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.home.join(rel)
    }

    fn ddc(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
            .clear_env()
            .home_env(&self.home)
            .env("PATH", "")
            .env("NO_COLOR", "1")
            .args(args)
            .arg("--data-dir")
            .arg(&self.data)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    /// `ddc keep remove <rel> [--folder-dev --folder-ino] <extra>`.
    fn remove(&self, rel: &str, identity: Option<(u64, u64)>, extra: &[&str]) -> Output {
        let folder = self.path(rel).display().to_string();
        let mut args = vec!["keep".to_owned(), "remove".to_owned(), folder];
        if let Some((dev, ino)) = identity {
            args.extend(["--folder-dev".to_owned(), dev.to_string(), "--folder-ino".to_owned(), ino.to_string()]);
        }
        args.extend(extra.iter().map(|a| (*a).to_owned()));
        self.ddc(&args.iter().map(String::as_str).collect::<Vec<_>>())
    }

    /// `--json`, exit 0, the report valid against `docs/keep.schema.json`.
    fn remove_json(&self, rel: &str, identity: Option<(u64, u64)>) -> KeepReport {
        let out = self.remove(rel, identity, &["--json"]);
        assert_eq!(out.status.code(), Some(0), "{rel}: {}", String::from_utf8_lossy(&out.stderr));
        let report = contract::holds::<KeepReport>("keep", &out.stdout);
        assert_eq!(report.schema_version, KEEP_SCHEMA_VERSION);
        assert_eq!(report.path, self.path(rel));
        report
    }

    fn scan(&self) -> ScanReport {
        let out = self.ddc(&["scan", "--json"]);
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        serde_json::from_slice(&out.stdout).unwrap()
    }

    /// A scan, then its `last-scan.json` rewritten with [`REPORTED_RULE`] as
    /// the rule each marker kept.
    fn saved_report(&self) {
        let mut report: Value = serde_json::to_value(self.scan()).unwrap();
        let mut kept = 0;
        for f in report["findings"].as_array_mut().unwrap() {
            if let Some(protection) = f.get_mut("protection") {
                protection["rule"] = REPORTED_RULE.into();
                kept += 1;
            }
        }
        assert!(kept >= 2, "the scan kept both node_modules: {report}");
        fs::write(self.data.join("last-scan.json"), serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }

    fn settings(&self, json: Value) {
        fs::create_dir_all(&self.data).unwrap();
        fs::write(self.data.join("settings.json"), json.to_string()).unwrap();
    }

    fn journal(&self) -> Vec<Value> {
        fs::read_to_string(self.data.join("logs/actions.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn env(&self) -> Env {
        Env::with_paths(&self.home, &self.data, &self.data.join("logs"), SystemTime::now()).unwrap()
    }

    fn marker(&self, rel: &str) -> PathBuf {
        self.path(rel).join(".ddc-keep")
    }
}

/// One journal line: kind `keep-remove`, version 5, the folder, its identity
/// and the rule from the saved report.
fn assert_line(line: &Value, report: &KeepReport) {
    assert_eq!(line["schema_version"], 6, "{line}");
    assert_eq!(line["kind"], "keep-remove");
    assert_eq!(line["path"], report.path.to_str().unwrap());
    assert_eq!(line["dev"], serde_json::to_value(report.dev).unwrap());
    assert_eq!(line["ino"], serde_json::to_value(report.ino).unwrap());
    assert_eq!(line["rule"], serde_json::to_value(&report.rule).unwrap());
    let result = serde_json::to_value(&report.result).unwrap();
    assert_eq!(line["result"], result["result"]);
    assert_eq!(line.get("reason"), result.get("reason"));
    assert!(line.get("class").is_none() && line.get("bytes").is_none(), "{line}");
}

/// AC2, AC3, AC5: a regular-file marker goes, with a journal line; the
/// folder stays, and the next scan lists it in its own group.
#[test]
fn a_regular_marker_is_removed_journaled_and_the_folder_returns_to_its_group() {
    if common::unsupported("a_regular_marker_is_removed_journaled_and_the_folder_returns_to_its_group") {
        return;
    }
    let fx = Fx::new();
    fx.saved_report();
    let id = common::id(fx.path("a/node_modules"));

    let report = fx.remove_json("a/node_modules", Some(id));
    assert_eq!(report.result, KeepResult::Removed);
    assert_eq!((report.dev, report.ino), (Some(id.0), Some(id.1)));
    assert_eq!(report.rule.as_deref(), Some(REPORTED_RULE), "the rule comes from the saved report");
    assert!(!fx.marker("a/node_modules").exists(), "the marker is gone");
    assert!(fx.path("a/node_modules/dep/index.js").exists(), "nothing else is");
    let journal = fx.journal();
    assert_eq!(journal.len(), 1);
    assert_line(&journal[0], &report);

    let after = fx.scan();
    let finding = |rel: &str| after.findings.iter().find(|f| f.path == fx.path(rel)).unwrap();
    let freed = finding("a/node_modules");
    assert_eq!(freed.rule, "node.node_modules", "listed in its own group");
    assert_eq!(freed.protection, None);
    assert_eq!(finding("b/node_modules").rule, "user.kept", "the other marker still protects");

    // In text mode, one line and exit 0.
    let b = common::id(fx.path("b/node_modules"));
    let out = fx.remove("b/node_modules", Some(b), &[]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "Removed .ddc-keep from ~/b/node_modules\n");
}

/// AC2, AC4, AC5: every refusal under the lock is in the report with exit 0
/// under `--json`, journaled with its reason; the marker stays.
#[test]
fn every_refusal_under_the_lock_is_reported_and_journaled() {
    if common::unsupported("every_refusal_under_the_lock_is_reported_and_journaled") {
        return;
    }
    let fx = Fx::new();
    // `b` is outside the roots.
    fx.settings(serde_json::json!({ "schema_version": 1, "roots": [fx.path("a"), fx.path("c")] }));
    // A symlink named `.ddc-keep`, and a folder of that name.
    fx.write("c/x/target", b"");
    fs::create_dir_all(fx.path("c/y/.ddc-keep")).unwrap();
    common::symlink(fx.path("c/x/target"), fx.marker("c/x"));
    let a = common::id(fx.path("a/node_modules"));
    let refused = |reason| KeepResult::Refused { reason };

    let cases = [
        ("a/node_modules", (a.0, a.1 + 1), KeepRefusal::ChangedSinceScan),
        ("c/x", common::id(fx.path("c/x")), KeepRefusal::NotAFile),
        ("c/y", common::id(fx.path("c/y")), KeepRefusal::NotAFile),
        ("b/node_modules", common::id(fx.path("b/node_modules")), KeepRefusal::OutsideScanRoots),
        ("c/gone", (1, 2), KeepRefusal::NoFolder),
    ];
    let mut reports = Vec::new();
    for (rel, identity, reason) in cases {
        let report = fx.remove_json(rel, Some(identity));
        assert_eq!(report.result, refused(reason), "{rel}");
        assert_eq!(report.rule, None, "no saved report: no rule");
        reports.push(report);
    }
    assert!(fx.marker("a/node_modules").exists() && fx.marker("b/node_modules").exists());
    assert!(fx.marker("c/x").is_symlink(), "the symlink marker still protects");
    assert!(fx.marker("c/y").is_dir(), "the folder marker still protects");
    let journal = fx.journal();
    assert_eq!(journal.len(), reports.len(), "one line per decision");
    for (line, report) in journal.iter().zip(&reports) {
        assert_line(line, report);
    }
    assert_eq!(reports[0].ino, Some(a.1 + 1), "the identity given, as the report recorded it");

    // Without --json a refusal exits 1, and says what to do.
    let out = fx.remove("c/y", Some(common::id(fx.path("c/y"))), &[]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "Refused: ~/c/y/.ddc-keep is not a file (a symlink or a folder); remove it in Finder\n"
    );
}

/// AC4: a folder with no marker any more is `absent`, journaled, exit 0, in
/// text mode too.
#[test]
fn a_folder_without_a_marker_is_absent() {
    if common::unsupported("a_folder_without_a_marker_is_absent") {
        return;
    }
    let fx = Fx::new();
    // Removed in Finder, say.
    let id = common::id(fx.path("a/node_modules/dep"));
    let report = fx.remove_json("a/node_modules/dep", Some(id));
    assert_eq!(report.result, KeepResult::Absent);
    assert_eq!((report.dev, report.ino), (Some(id.0), Some(id.1)));
    let out = fx.remove("a/node_modules/dep", Some(id), &[]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "No .ddc-keep in ~/a/node_modules/dep\n");
    let journal = fx.journal();
    assert_eq!(journal.len(), 2);
    assert_line(&journal[0], &report);
}

/// AC2, AC4: a held lock exits 75 and writes no line; without the identity
/// and without a terminal, and for a malformed call, it exits 2 before the
/// lock. Nothing is removed and the journal is never opened.
#[test]
fn a_held_lock_or_a_call_refused_before_the_lock_removes_and_journals_nothing() {
    if common::unsupported("a_held_lock_or_a_call_refused_before_the_lock_removes_and_journals_nothing") {
        return;
    }
    let fx = Fx::new();
    let id = common::id(fx.path("a/node_modules"));
    {
        // A scan or a clean holds it.
        let _held = store::try_lock(&fx.env(), LockMode::Shared).unwrap();
        let out = fx.remove("a/node_modules", Some(id), &["--json"]);
        assert_eq!(out.status.code(), Some(75), "{}", String::from_utf8_lossy(&out.stderr));
        assert!(out.stdout.is_empty());
    }

    let out = fx.remove("a/node_modules", None, &["--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("confirmation needs a terminal"));
    assert!(out.stdout.is_empty());

    let relative = fx.ddc(&["keep", "remove", "a/node_modules", "--folder-dev", "1", "--folder-ino", "2"]);
    assert_eq!(relative.status.code(), Some(2), "{}", String::from_utf8_lossy(&relative.stderr));
    let dotted = fx.path("a/../a/node_modules").display().to_string();
    let dotted = fx.ddc(&["keep", "remove", &dotted, "--folder-dev", "1", "--folder-ino", "2"]);
    assert_eq!(dotted.status.code(), Some(2));
    let half = fx.ddc(&["keep", "remove", fx.path("a/node_modules").to_str().unwrap(), "--folder-dev", "1"]);
    assert_eq!(half.status.code(), Some(2), "--folder-dev needs --folder-ino");

    assert!(fx.marker("a/node_modules").exists());
    assert!(!fx.data.join("logs").exists(), "the journal was never opened");
}

/// AC5: a saved report of another schema version gives no rule.
#[test]
fn a_saved_report_of_another_version_gives_no_rule() {
    if common::unsupported("a_saved_report_of_another_version_gives_no_rule") {
        return;
    }
    let fx = Fx::new();
    fx.saved_report();
    let path = fx.data.join("last-scan.json");
    let mut saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    saved["schema_version"] = 1.into();
    fs::write(&path, saved.to_string()).unwrap();
    let report = fx.remove_json("a/node_modules", Some(common::id(fx.path("a/node_modules"))));
    assert_eq!((&report.result, report.rule.as_deref()), (&KeepResult::Removed, None));
}
