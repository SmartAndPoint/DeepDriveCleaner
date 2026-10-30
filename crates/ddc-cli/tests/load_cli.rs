//! The load harness end to end (story "Memory and CPU under load: peak
//! memory, leaks and CPU time"): an S `projects` tree in a marked temporary
//! folder, every reading command measured once through `ddc-loadgen run`.
//! A temporary target gets no clean.

use std::path::{Path, PathBuf};

use ddc_loadgen::limits::{self, Mode};
use ddc_loadgen::profiles::Profile;
use ddc_loadgen::run::{RunArgs, run};
use ddc_loadgen::table::{COLUMNS, Meta};
use ddc_loadgen::target;

mod common;

fn args(into: &Path, out: &Path, mode: Mode, limits: PathBuf) -> RunArgs {
    RunArgs {
        profile: Profile::Projects,
        entries: 10_000,
        seed: 1,
        into: into.to_owned(),
        device: None,
        cli: PathBuf::from(env!("CARGO_BIN_EXE_deepdrivecleaner")),
        repeat: 1,
        size: "S".into(),
        date: "2026-10-29".into(),
        commit: "test".into(),
        out: out.to_owned(),
        mode,
        limits,
        load_start: None,
    }
}

#[test]
fn a_harness_run_measures_every_reading_command() {
    if common::unsupported("a_harness_run_measures_every_reading_command") {
        return;
    }
    let dir = target::Scratch::new().unwrap();
    let out = tempfile::tempdir().unwrap();
    let report = run(&args(dir.path(), out.path(), Mode::Record, PathBuf::from("/nonexistent"))).unwrap();
    assert!(report.passed(), "{:?} {:?}", report.differences, report.failures);
    assert!(report.clean.is_none(), "a temporary target is never cleaned");

    let commands: Vec<&str> = report.rows.iter().map(|r| r.command.as_str()).collect();
    assert_eq!(commands, ["scan", "clean-dry-run", "scan-background"]);
    for r in &report.rows {
        assert_eq!(r.exit, "0", "{r:?}");
        assert!(r.max_rss_bytes > 1 << 20, "{r:?}");
        assert!(r.threads_max >= 1, "{r:?}");
        assert!(r.wall_ms > 0, "{r:?}");
    }
    // The scan's own CPU time, not the harness's.
    assert!(report.rows[0].user_ms > 0, "{:?}", report.rows[0]);

    let tsv = std::fs::read_to_string(report.tsv.as_ref().unwrap()).unwrap();
    let lines: Vec<&str> = tsv.lines().collect();
    let meta = Meta::parse(lines[0]).expect("# line");
    assert_eq!((meta.commit.as_str(), meta.mode.as_str(), meta.seed), ("test", "record", 1));
    assert_eq!(lines[1].split('\t').collect::<Vec<_>>(), COLUMNS);
    assert_eq!(lines.len(), 2 + 3);
    assert!(lines[2..].iter().all(|l| l.split('\t').count() == COLUMNS.len()));
}

/// `enforce` judges the medians against the limits file: a peak memory over
/// its limit is a failure, and so is a command with no limit.
#[test]
fn an_enforced_run_fails_over_a_limit() {
    if common::unsupported("an_enforced_run_fails_over_a_limit") {
        return;
    }
    let dir = target::Scratch::new().unwrap();
    let out = tempfile::tempdir().unwrap();
    let file = out.path().join("limits.tsv");
    let tiny = limits::Limit { rss_base: 1.0, rss_per_entry: 0.0, cpu_base_ms: 1e9, cpu_ms_per_entry: 0.0 };
    let set: limits::Limits = [("scan", tiny), ("clean-dry-run", tiny)]
        .into_iter()
        .map(|(c, l)| ((c.to_owned(), "projects".to_owned()), l))
        .collect();
    std::fs::write(&file, limits::render(&set, "test")).unwrap();
    let report = run(&args(dir.path(), out.path(), Mode::Enforce, file)).unwrap();
    assert!(!report.passed());
    let f = &report.failures;
    assert_eq!(f.len(), 3, "{f:?}");
    assert!(f.iter().any(|l| l.starts_with("scan on projects: peak memory")), "{f:?}");
    assert!(f.iter().any(|l| l.starts_with("clean-dry-run on projects: peak memory")), "{f:?}");
    assert!(f.iter().any(|l| l.starts_with("scan-background on projects: no limit")), "{f:?}");
    let tsv = std::fs::read_to_string(report.tsv.as_ref().unwrap()).unwrap();
    let mode = Meta::parse(tsv.lines().next().unwrap()).unwrap().mode;
    assert_eq!(mode, if report.cpu_judged { "enforce" } else { "enforce-memory" });
}
