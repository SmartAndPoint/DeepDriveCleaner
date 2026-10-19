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
