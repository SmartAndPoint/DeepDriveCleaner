//! `ddc rules --json`: the Types settings' source (story "CLI: shared
//! settings, types and the notify decision"); `--platform` prints another
//! platform's catalog (story "Rule catalog split: shared and per-OS sections").

use std::process::{Command, Output};

use ddc_core::platform::Platform;
use ddc_core::rules::listing::RuleList;
use ddc_core::rules::{RULES, SHARED_IDS, rules_of};
use ddc_loadgen::isolate::ClearEnv as _;

mod contract;

fn ddc(args: &[&str]) -> Output {
    let tmp = tempfile::tempdir().unwrap();
    Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .clear_env()
        .env("HOME", tmp.path())
        .env("NO_COLOR", "1")
        .args(args)
        .output()
        .unwrap()
}

/// The text of `ddc rules --platform <os>`, which must succeed.
fn text_of(os: &str) -> String {
    let out = ddc(&["rules", "--platform", os]);
    assert!(out.status.success(), "{os}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

/// The ids a `ddc rules` text lists: the first word of each rule line.
fn ids_in(text: &str) -> Vec<&str> {
    text.lines()
        .filter(|l| l.starts_with("  ") && !l.starts_with("   "))
        .filter_map(|l| l.split_whitespace().next())
        .collect()
}

/// The `~/…` lines printed under rule `id`.
fn paths_under<'a>(text: &'a str, id: &str) -> Vec<&'a str> {
    let mut lines = text.lines().skip_while(|l| l.split_whitespace().next() != Some(id) || !l.starts_with("  "));
    lines.next().expect("the rule is listed");
    lines.take_while(|l| l.starts_with("      ")).map(str::trim).collect()
}

#[test]
fn rules_json_lists_every_rule_with_its_group() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .clear_env()
        .env("HOME", tmp.path())
        .args(["rules", "--json"])
        .output()
        .unwrap();

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let list: RuleList = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(serde_json::to_value(&list).unwrap(), json, "every field is in the contract");
    assert_eq!(list, RuleList::builtin());
    assert_eq!(list.rules.len(), RULES.len());
    assert!(list.rules.iter().all(|r| list.groups.contains(&r.group)));
}

/// The groups are the Types rows of the revised Settings (story "Settings:
/// folders, types, notifications", D2): "Package downloads" are caches a
/// package manager downloads again, and "Toolchain caches" holds only `safe`
/// rules, so the `review` downloads have a row of their own. The macOS
/// catalog, on every OS the tests run on.
#[test]
fn rules_json_groups_follow_the_settings_types() {
    let list = RuleList::for_platform(ddc_core::platform::Platform::Macos);
    assert_eq!(
        list.groups,
        [
            "Python environments",
            "node_modules",
            "Build outputs",
            "Build caches in projects",
            "Package downloads",
            "Toolchain caches",
            "Maven and test browsers",
            "Browser and app caches",
            "IDE caches",
            "Xcode builds",
            "Xcode device support",
            "Old toolchains",
            "ML models",
            "VM and container images",
            "Interrupted cleanups",
            "Trash",
            "Your data",
        ]
    );
    let members = |group: &str| {
        let mut ids: Vec<&str> = list.rules.iter().filter(|r| r.group == group).map(|r| r.id.as_str()).collect();
        ids.sort_unstable();
        ids
    };
    assert_eq!(
        members("Package downloads"),
        [
            "cache.bun",
            "cache.cocoapods",
            "cache.conda",
            "cache.homebrew",
            "cache.npm",
            "cache.pip",
            "cache.pnpm",
            "cache.poetry",
            "cache.yarn"
        ]
    );
    assert_eq!(
        members("Toolchain caches"),
        ["cache.cargo", "cache.go-build", "cache.go-mod", "cache.gradle", "cache.uv"]
    );
    assert_eq!(
        members("Maven and test browsers"),
        ["browsers.cypress", "browsers.playwright", "browsers.puppeteer", "repo.maven"]
    );
    assert_eq!(members("Trash"), ["trash"], "the Trash has its own group");
    assert_eq!(list.rules.iter().filter(|r| r.id == "trash").count(), 1, "and is in no other");
    for group in ["Package downloads", "Toolchain caches"] {
        assert!(
            list.rules.iter().filter(|r| r.group == group).all(|r| r.class == ddc_core::class::RiskClass::Safe),
            "{group} is all safe"
        );
    }
}

/// Linux and Windows print every shared rule and no macOS-only one.
#[test]
fn every_platform_lists_the_shared_rules_and_only_its_own() {
    for os in ["linux", "windows"] {
        let text = text_of(os);
        let ids = ids_in(&text);
        for id in SHARED_IDS {
            assert!(ids.contains(&id), "{os} lists {id}");
        }
        assert!(!ids.iter().any(|id| id.starts_with("xcode.")), "{os} lists no Xcode rule:\n{text}");
    }
    let macos = text_of("macos");
    assert_eq!(ids_in(&macos).len(), rules_of(Platform::Macos).len(), "the macOS text lists every macOS rule");
}

/// A shared rule's folder is the platform's own: pip's cache is in
/// `~/.cache` on Linux and somewhere else on Windows. The Windows folder is
/// not pinned here; the catalog cites where it comes from.
#[test]
fn the_text_shows_each_platforms_paths() {
    let linux = text_of("linux");
    let windows = text_of("windows");
    assert_eq!(paths_under(&linux, "cache.pip"), ["~/.cache/pip"]);
    let on_windows = paths_under(&windows, "cache.pip");
    assert_eq!(on_windows.len(), 1, "{windows}");
    assert_ne!(on_windows, paths_under(&linux, "cache.pip"));
    assert_eq!(paths_under(&text_of("macos"), "cache.pip"), ["~/Library/Caches/pip"]);
}

/// Another platform's JSON keeps the contract: valid against the schema and
/// read back unchanged, with that platform's rules.
#[test]
fn another_platforms_json_holds_the_contract() {
    let out = ddc(&["rules", "--platform", "linux", "--json"]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let list = contract::holds::<RuleList>("rules", &out.stdout);
    assert_eq!(list, RuleList::for_platform(Platform::Linux));
    assert_eq!(list.rules.len(), rules_of(Platform::Linux).len());
    assert!(list.rules.iter().all(|r| list.groups.contains(&r.group)));
}

#[test]
fn an_unknown_platform_is_a_usage_error() {
    let out = ddc(&["rules", "--platform", "nope"]);
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
}
