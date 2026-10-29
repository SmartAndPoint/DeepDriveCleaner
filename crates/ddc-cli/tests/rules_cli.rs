//! `ddc rules --json`: the Types settings' source (story "CLI: shared
//! settings, types and the notify decision").

use std::process::Command;

use ddc_core::rules::RULES;
use ddc_core::rules::listing::RuleList;

#[test]
fn rules_json_lists_every_rule_with_its_group() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .env_clear()
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
/// rules, so the `review` downloads have a row of their own.
#[test]
fn rules_json_groups_follow_the_settings_types() {
    let list = RuleList::builtin();
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
