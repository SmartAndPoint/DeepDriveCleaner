//! The load harness end to end (story "Memory and CPU under load: peak
//! memory, leaks and CPU time"): an S `projects` tree in a marked temporary
//! folder, every reading command measured once through `ddc-loadgen run`.
//! A temporary target gets no clean.

use std::path::PathBuf;

use ddc_loadgen::profiles::Profile;
use ddc_loadgen::run::{RunArgs, run};
use ddc_loadgen::table::{COLUMNS, Meta};
use ddc_loadgen::target;

#[test]
fn a_harness_run_measures_every_reading_command() {
    let dir = target::Scratch::new().unwrap();
    let out = tempfile::tempdir().unwrap();
    let a = RunArgs {
        profile: Profile::Projects,
        entries: 10_000,
        seed: 1,
        into: dir.path().to_owned(),
        device: None,
        cli: PathBuf::from(env!("CARGO_BIN_EXE_deepdrivecleaner")),
        repeat: 1,
        size: "S".into(),
        date: "2026-10-29".into(),
        commit: "test".into(),
        out: out.path().to_owned(),
    };
    let report = run(&a).unwrap();
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
