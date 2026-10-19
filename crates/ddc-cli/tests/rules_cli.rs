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
