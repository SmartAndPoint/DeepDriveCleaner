//! The background path cannot reach the cleanup executor (ADR "Cleanup safety
//! model", §8): its sources do not mention it.

use std::path::Path;

#[test]
fn background_sources_never_reference_cleanup() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for file in ["cmd/background.rs", "cmd/notify.rs", "notify.rs", "launchd.rs"] {
        let text = std::fs::read_to_string(src.join(file)).unwrap();
        let code: String = text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        for forbidden in ["exec::", "plan::", "Executor", "build_plan", "trash", "remove_dir_all", "Consent"] {
            assert!(!code.contains(forbidden), "{file} mentions {forbidden}");
        }
    }
}

/// The background scan reaches osascript only through the `Notifier` the
/// dispatcher hands it, so a test double sees every notification.
#[test]
fn the_background_scan_runs_no_program_itself() {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cmd/background.rs")).unwrap();
    let code: String = text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    for forbidden in ["Osascript", "osascript", "Command::new", "process::Command", "/usr/bin/"] {
        assert!(!code.contains(forbidden), "cmd/background.rs mentions {forbidden}");
    }
}

/// `ddc disk` only measures (story "Whole disk: what lies outside the home
/// folder"): its sources reach no cleanup, start no program themselves, and
/// never run `xcrun` or a first launch, which can ask for a password.
#[test]
fn the_disk_report_never_reaches_cleanup() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = vec![root.join("ddc-cli/src/cmd/disk.rs")];
    for entry in std::fs::read_dir(root.join("ddc-core/src/disk")).unwrap() {
        files.push(entry.unwrap().path());
    }
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        let code: String = text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        for forbidden in [
            "Executor",
            "build_plan",
            "plan::",
            "trash",
            "Consent",
            "remove_",
            "Command::new",
            "xcrun",
            "runFirstLaunch",
            "\"delete\"",
        ] {
            assert!(!code.contains(forbidden), "{} mentions {forbidden}", file.display());
        }
    }
}
