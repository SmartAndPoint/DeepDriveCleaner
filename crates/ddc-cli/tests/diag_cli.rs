//! Process setup done by `main` before any filesystem access.

use ddc_core::platform::Capability;
use ddc_loadgen::isolate::ClearEnv as _;
use std::process::Command;

mod common;

#[test]
fn ddc_starts_with_dataless_materialization_off_and_raised_file_limit() {
    if common::unsupported("ddc_starts_with_dataless_materialization_off_and_raised_file_limit")
        || common::lacks(
            "ddc_starts_with_dataless_materialization_off_and_raised_file_limit",
            Capability::DatalessFiles,
        )
    {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .clear_env()
        .env("HOME", tmp.path())
        .current_dir(tmp.path())
        .arg("diag")
        .output()
        .unwrap();

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.lines().any(|l| l == "dataless_materialization: off"), "{stdout}");
    let limit: u64 =
        stdout.lines().find_map(|l| l.strip_prefix("open_files_limit: ")).and_then(|v| v.parse().ok()).unwrap();
    assert!(limit > 256, "open-file limit not raised: {limit}");
    assert!(stdout.contains("volume: apfs"), "{stdout}");
    let snapshot = stdout.lines().find_map(|l| l.strip_prefix("process_snapshot: ")).expect("the snapshot line");
    let numbers: Vec<u64> = snapshot.split(|c: char| !c.is_ascii_digit()).filter_map(|n| n.parse().ok()).collect();
    assert!(
        snapshot.ends_with(" ms") && numbers.len() == 3 && numbers[0] > 0,
        "processes, open files and milliseconds: {snapshot}"
    );
}
