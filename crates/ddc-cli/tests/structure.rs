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

/// `ddc disk` and `ddc disk agents` only measure (story "Whole disk: what
/// lies outside the home folder"): its sources reach no cleanup, start no program themselves, and
/// never run `xcrun` or a first launch, which can ask for a password.
#[test]
fn the_disk_report_never_reaches_cleanup() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = vec![root.join("ddc-cli/src/cmd/disk.rs"), root.join("ddc-cli/src/cmd/disk_agents.rs")];
    // `ddc disk agents` too (story "Coding agents in the Whole disk view").
    for dir in ["ddc-core/src/disk", "ddc-core/src/agents"] {
        for entry in std::fs::read_dir(root.join(dir)).unwrap() {
            files.push(entry.unwrap().path());
        }
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

/// `ddc disk free` runs only the two commands the safety ADR names
/// (amendment of 2026-10-28): no item cleanup, no program started by hand,
/// no `xcrun` or first launch, no `simctl runtime delete --keep-asset`, no
/// `--prune`, no `sudo`, and no Homebrew `autoremove`.
#[test]
fn disk_free_runs_only_its_two_commands() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = vec![root.join("ddc-cli/src/cmd/disk_free.rs")];
    for entry in std::fs::read_dir(root.join("ddc-core/src/free")).unwrap() {
        files.push(entry.unwrap().path());
    }
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        for forbidden in [
            "Executor",
            "build_plan",
            "trash",
            "Consent",
            "remove_",
            "Command::new",
            "xcrun",
            "runFirstLaunch",
            "keep-asset",
            "prune",
            "sudo",
            "\"autoremove\"",
        ] {
            assert!(!text.contains(forbidden), "{} mentions {forbidden}", file.display());
        }
    }
}

/// The cost counters of the complexity guard (story "Complexity guard:
/// operation counts and scaling exponents") never reach a release: the dist
/// build passes no features, `op-stats` is no default, and no manifest turns
/// it on for a dependency, where feature unification would spread it.
#[test]
fn the_release_build_carries_no_counters() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dist = std::fs::read_to_string(root.join("scripts/build-dist.sh")).unwrap();
    let build: Vec<&str> = dist.lines().filter(|l| l.contains("\"$CARGO\" build")).collect();
    assert_eq!(build.len(), 1, "scripts/build-dist.sh: {build:?}");
    for flag in ["--features", "-F", "--all-features"] {
        assert!(
            !build[0].split_whitespace().any(|w| w == flag || w.starts_with("--features=")),
            "{flag}: {}",
            build[0]
        );
    }
    for manifest in
        ["Cargo.toml", "crates/ddc-core/Cargo.toml", "crates/ddc-cli/Cargo.toml", "crates/ddc-loadgen/Cargo.toml"]
    {
        let text = std::fs::read_to_string(root.join(manifest)).unwrap();
        let mut section = "";
        for line in text.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
            if line.starts_with('[') {
                section = line;
                continue;
            }
            if line.contains("op-stats") {
                assert_eq!(section, "[features]", "{manifest}: {line}");
                assert!(!line.starts_with("default"), "{manifest}: {line}");
            }
        }
    }
}

/// Every directory read in the core is counted: it goes through
/// `stats::read_dir` or a `DirReader`, never `fs::read_dir` directly.
#[test]
fn the_core_reads_directories_only_through_the_counters() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ddc-core/src");
    let mut dirs = vec![src];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") || path.ends_with("stats.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for line in text.lines().filter(|l| !l.trim_start().starts_with("//")) {
                assert!(!line.contains("fs::read_dir("), "{}: {line}", path.display());
            }
        }
    }
}
