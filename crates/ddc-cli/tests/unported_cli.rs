//! The CLI of a build with `SUPPORTED` false (ADR "Platform seam: one
//! platform module per OS with declared capabilities", §5, §7, §8): built
//! only by `make unported-check`, with the `simulate-unported` feature.
//! `--version`, `--help` and `rules` run; every other command exits 69
//! before it reads the disk, with the error envelope on stdout when its JSON
//! output was asked for. The temporary home is left as it was.
#![cfg(feature = "simulate-unported")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use ddc_core::refusal::{ErrorCode, ErrorReport};
use ddc_core::rules::listing::RuleList;
use ddc_loadgen::isolate::ClearEnv as _;

mod contract;

/// `EX_UNAVAILABLE`, as `exit::UNSUPPORTED` names it.
const UNSUPPORTED: i32 = 69;

fn ddc(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .clear_env()
        .env("HOME", home)
        .env("PATH", "")
        .env("NO_COLOR", "1")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// A home with a project whose `node_modules` a scan would find.
fn home() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().canonicalize().unwrap().join("home");
    for (rel, content) in [("p/package.json", &b"{}"[..]), ("p/node_modules/x/index.js", &[1u8; 4_000][..])] {
        let p = home.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }
    (tmp, home)
}

/// Every path under `dir`, sorted.
fn tree(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() && !path.is_symlink() {
            out.extend(tree(&path));
        }
        out.push(path);
    }
    out.sort();
    out
}

/// Every command but `rules`, each kind of JSON output once.
const REFUSED: &[(&[&str], bool)] = &[
    (&["scan"], false),
    (&["scan", "--json"], true),
    (&["scan", "--background"], false),
    (&["scan", "--background", "--notify", "json"], true),
    (&["clean", "--dry-run"], false),
    (&["clean", "--items", "-", "--json"], true),
    (&["du"], false),
    (&["disk"], false),
    (&["disk", "--json"], true),
    (&["disk", "agents", "--json"], true),
    (&["disk", "free", "--json"], true),
    (&["disk", "brew-packages", "--json"], true),
    (&["schedule", "status"], false),
    (&["notify", "ack", "--bytes", "1"], false),
    (&["diag"], false),
];

#[test]
fn every_command_but_rules_is_refused_with_69() {
    let (tmp, home) = home();
    let before = tree(tmp.path());
    for (args, json) in REFUSED {
        let out = ddc(&home, args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(UNSUPPORTED), "{args:?}: {stderr}");
        assert_eq!(
            stderr.trim_end(),
            "ddc: this build was made with simulate-unported and refuses every scan and clean: \
             only --version, --help and rules run",
            "{args:?}"
        );
        if *json {
            let report = contract::holds::<ErrorReport>("error", &out.stdout);
            assert_eq!(report.error_schema_version, ddc_core::refusal::ERROR_SCHEMA_VERSION);
            assert_eq!(report.error.code, ErrorCode::PlatformNotSupported, "{args:?}");
            assert_eq!(report.error.platform, Some(ddc_core::platform::PLATFORM), "{args:?}");
            assert!(report.error.simulated, "{args:?}");
            assert_eq!(report.error.capability, None);
            assert_eq!(report.error.action, None);
        } else {
            assert!(out.stdout.is_empty(), "{args:?}: {}", String::from_utf8_lossy(&out.stdout));
        }
    }
    assert_eq!(tree(tmp.path()), before, "nothing was read into or written under the home");
}

#[test]
fn version_help_and_rules_run() {
    let (tmp, home) = home();
    let before = tree(tmp.path());
    for args in [&["--version"][..], &["--help"], &["scan", "--help"], &["disk", "free", "--help"], &["rules"]] {
        let out = ddc(&home, args);
        assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(!out.stdout.is_empty(), "{args:?}");
    }
    let out = ddc(&home, &["rules", "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    contract::holds::<RuleList>("rules", &out.stdout);
    assert_eq!(tree(tmp.path()), before);
}

#[test]
fn a_usage_error_is_still_2() {
    let (_tmp, home) = home();
    let out = ddc(&home, &["scan", "--no-such-flag"]);
    assert_eq!(out.status.code(), Some(2));
}
