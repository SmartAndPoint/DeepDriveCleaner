//! The fuzz targets' inputs as tests (story "Coverage floor and property tests
//! for the safety invariants", DDC-005): every file in
//! `fuzz/regressions/<target>/`, a crash `make fuzz` found or a hand-picked
//! hostile input, goes to its parser, which returns without a panic and, where
//! the target checks it, reads back what it accepted once written.

use std::fs;
use std::panic::{self, UnwindSafe};
use std::path::{Path, PathBuf};

use ddc_core::disk::plist;
use ddc_core::free::ActionList;
use ddc_core::plan::ListedItems;
use ddc_core::{settings, store};

/// The files of one target, sorted.
fn inputs(target: &str) -> Vec<(PathBuf, Vec<u8>)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/regressions").join(target);
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    assert!(paths.len() >= 3, "{} holds too few inputs to test anything", dir.display());
    paths
        .into_iter()
        .map(|p| {
            let bytes = fs::read(&p).unwrap();
            (p, bytes)
        })
        .collect()
}

/// Runs `check` on each input of `target`, naming the one that panics, and
/// returns how many it accepted.
fn each(target: &str, check: impl Fn(&[u8]) -> bool + Copy + UnwindSafe) -> usize {
    let mut accepted = 0;
    for (path, bytes) in inputs(target) {
        match panic::catch_unwind(move || check(&bytes)) {
            Ok(ok) => accepted += usize::from(ok),
            Err(_) => panic!("{} panicked on {}", target, path.display()),
        }
    }
    accepted
}

#[test]
fn the_plist_reader_returns_on_every_kept_input() {
    // Text only: the command runner reads stdout with `read_to_string`, so bytes
    // that are not UTF-8 never reach the reader, as in the fuzz target.
    let accepted = each("disk_plist", |bytes| std::str::from_utf8(bytes).is_ok_and(|text| plist::parse(text).is_ok()));
    assert!(accepted >= 1, "no input reads as a property list");
}

#[test]
fn the_item_list_returns_on_every_kept_input_and_reads_back_what_it_accepts() {
    let accepted = each("listed_items", |bytes| {
        let Ok(list) = ListedItems::parse(bytes) else { return false };
        assert_eq!(ListedItems::parse(&serde_json::to_vec(&list).unwrap()), Ok(list));
        true
    });
    assert!(accepted >= 1, "no input reads as an item list");
}

#[test]
fn the_action_list_returns_on_every_kept_input_and_reads_back_what_it_accepts() {
    let accepted = each("action_list", |bytes| {
        let Ok(list) = ActionList::parse(bytes) else { return false };
        assert_eq!(ActionList::parse(&serde_json::to_vec(&list).unwrap()), Ok(list));
        true
    });
    assert!(accepted >= 1, "no input reads as an action list");
}

#[test]
fn settings_return_on_every_kept_input_and_read_back_what_they_accept() {
    let accepted = each("settings", |bytes| {
        let Ok(s) = settings::parse(bytes) else { return false };
        assert_eq!(settings::parse(&serde_json::to_vec(&s).unwrap()), Ok(s));
        true
    });
    assert!(accepted >= 1, "no input reads as settings");
}

#[test]
fn the_last_scan_returns_on_every_kept_input_and_reads_back_what_it_accepts() {
    let accepted = each("last_scan", |bytes| {
        let Ok(Some(report)) = store::decode_last_scan(bytes) else { return false };
        assert_eq!(store::decode_last_scan(&serde_json::to_vec(&report).unwrap()).unwrap(), Some(report));
        true
    });
    assert!(accepted >= 1, "no input reads as a scan report");
}

#[test]
fn deep_nesting_is_refused_without_overflowing_the_stack() {
    // Generated rather than kept: 100 000 levels is 700 KB of plist.
    let depth = 100_000;
    let plist = format!("<plist>{}{}</plist>", "<array>".repeat(depth), "</array>".repeat(depth));
    assert!(plist::parse(&plist).is_err());
    let json = |prefix: &str| format!("{prefix}{}{}}}", "[".repeat(depth), "]".repeat(depth)).into_bytes();
    assert!(ListedItems::parse(&json(r#"{"schema_version": 1, "items": "#)).is_err());
    assert!(ActionList::parse(&json(r#"{"schema_version": 3, "actions": "#)).is_err());
    assert!(settings::parse(&json(r#"{"schema_version": 1, "roots": "#)).is_err());
    // Too deep for the version check too: no last scan, as for any file that is not JSON.
    assert!(matches!(store::decode_last_scan(&json(r#"{"schema_version": 1, "findings": "#)), Ok(None)));
}
