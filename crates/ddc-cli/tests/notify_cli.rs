//! `ddc notify ack|snooze`: the app's calls that record a notification
//! (story "CLI: shared settings, types and the notify decision").

use ddc_core::platform::Canonical as _;
use ddc_loadgen::isolate::ClearEnv as _;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

mod common;

fn ddc(data: &Path, args: &[&str]) -> Output {
    let home = data.parent().unwrap();
    Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .clear_env()
        .home_env(home)
        .env("PATH", "")
        .args(args)
        .arg("--data-dir")
        .arg(data)
        .output()
        .unwrap()
}

fn state(data: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(data.join("notify-state.json")).unwrap()).unwrap()
}

#[test]
fn ack_records_the_notification_and_snooze_clears_it() {
    if common::unsupported("ack_records_the_notification_and_snooze_clears_it") {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().canonical().unwrap().join("data");

    let ack = ddc(&data, &["notify", "ack", "--bytes", "1234"]);
    assert!(ack.status.success() && ack.stdout.is_empty(), "{}", String::from_utf8_lossy(&ack.stderr));
    let s = state(&data);
    assert_eq!(s["last_bytes"], 1234);
    assert!(s["last_at"].as_u64().unwrap() > 1_700_000_000);
    assert!(s.get("snooze_until").is_none());

    let snooze = ddc(&data, &["notify", "snooze", "--until", "2030-01-02T03:04:05Z"]);
    assert!(snooze.status.success(), "{}", String::from_utf8_lossy(&snooze.stderr));
    assert_eq!(state(&data), serde_json::json!({ "last_at": null, "last_bytes": 0, "snooze_until": 1_893_553_445 }));

    ddc(&data, &["notify", "ack", "--bytes", "5"]);
    assert!(state(&data).get("snooze_until").is_none(), "an ack ends the snooze");
}

#[test]
fn a_time_that_is_not_utc_rfc_3339_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().canonical().unwrap().join("data");
    for until in ["tomorrow", "2030-01-02T03:04:05+02:00", "2030-01-02"] {
        let out = ddc(&data, &["notify", "snooze", "--until", until]);
        assert_eq!(out.status.code(), Some(2), "{until}");
    }
    assert!(!data.join("notify-state.json").exists());
}

#[test]
fn the_app_calls_are_hidden_from_help() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .clear_env()
        .home_env(tmp.path())
        .arg("--help")
        .output()
        .unwrap();
    let help = String::from_utf8(out.stdout).unwrap();
    let command = |name: &str| help.lines().any(|l| l.trim_start().starts_with(&format!("{name} ")));
    assert!(command("rules") && !command("notify"), "{help}");
}
