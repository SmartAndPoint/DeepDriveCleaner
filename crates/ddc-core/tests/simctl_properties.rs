//! A simulator runtime identifier matches exactly one runtime (story
//! "Coverage floor and property tests for the safety invariants", DDC-005).
//! Random `simctl runtime list -j` answers hold near-duplicate identifiers:
//! a case variant, a prefix, a longer one, one with a space or a newline,
//! the other Unicode spelling of the same letter, two runtimes of one id,
//! and the version, build and `runtimeIdentifier` the runtimes share. The
//! dry run, the delete and the list of what may run, all through a fake
//! `simctl`, must take exactly the runtime of that identifier or refuse.

mod common;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use common::fakes::{FakeCommands, FakeProcs};
use common::{TreeBuilder, reference_alloc};
use ddc_core::disk::{ActionId, Sources};
use ddc_core::exec::MemoryAreaJournal;
use ddc_core::free::{
    self, ActionList, ActionReport, ActionResult, FREE_SCHEMA_VERSION, FreeContext, ListedAction, Refusal,
};
use ddc_core::platform::Capability;
use proptest::prelude::*;
use proptest::sample::select;
use serde_json::{Value, json};

/// 256 cases unless `PROPTEST_CASES` says otherwise (proptest's default, read
/// from the environment); shrinking stops after 30 s unless
/// `PROPTEST_MAX_SHRINK_TIME` sets it.
fn config() -> ProptestConfig {
    let base = ProptestConfig::default();
    let max_shrink_time = if base.max_shrink_time == 0 { 30_000 } else { base.max_shrink_time };
    ProptestConfig { max_shrink_time, ..base }
}

const ASSETS: &str = "sys/System/Library/AssetsV2/com_apple_MobileAsset_iOSSimulatorRuntime";
const UUID: &str = "5C1A2B3D-0E4F-4A6B-8C9D-0123456789AB";

/// Identifiers a runtime may have: one UUID and its near misses.
fn identifiers() -> Vec<String> {
    vec![
        UUID.to_owned(),
        UUID.to_lowercase(),
        UUID[..8].to_owned(),
        UUID[..UUID.len() - 1].to_owned(),
        format!("{UUID}0"),
        format!("{UUID} "),
        format!(" {UUID}"),
        format!("{UUID}\n"),
        "5C1A2B3D-0E4F-4A6B-8C9D-0123456789AC".to_owned(),
        // The same letter, composed and decomposed.
        "runtime-\u{e9}".to_owned(),
        "runtime-e\u{301}".to_owned(),
    ]
}

/// What a runtime's other fields hold, and other words a list may name.
const RUNTIME_IDS: &[&str] = &[
    "com.apple.CoreSimulator.SimRuntime.iOS-27-1",
    "com.apple.CoreSimulator.SimRuntime.iOS-27-0",
    "com.apple.CoreSimulator.SimRuntime.watchOS-12-1",
];
const BUILDS: &[&str] = &["24A94425", "24A94426", "24B5055e"];
const VERSIONS: &[&str] = &["27.1", "27.0", "12.1"];

/// What a person may name: every identifier, and the other fields.
fn requests() -> Vec<String> {
    let mut all = identifiers();
    all.extend(RUNTIME_IDS.iter().chain(BUILDS).chain(VERSIONS).map(|s| (*s).to_owned()));
    all.extend(["", "all", "--dry-run"].map(str::to_owned));
    all
}

#[derive(Debug, Clone)]
struct Runtime {
    identifier: String,
    runtime_identifier: &'static str,
    build: &'static str,
    version: &'static str,
}

#[derive(Debug, Clone)]
struct Case {
    runtimes: Vec<Runtime>,
    /// A map keyed by identifier (Xcode 15 and later), or a list.
    map: bool,
    request: String,
}

fn runtime() -> impl Strategy<Value = Runtime> {
    (select(identifiers()), select(RUNTIME_IDS), select(BUILDS), select(VERSIONS)).prop_map(
        |(identifier, runtime_identifier, build, version)| Runtime { identifier, runtime_identifier, build, version },
    )
}

/// At most this many runtimes are listed.
const MAX_RUNTIMES: usize = 6;

fn cases() -> impl Strategy<Value = Case> {
    (prop::collection::vec(runtime(), 1..=MAX_RUNTIMES), any::<bool>(), select(requests()))
        .prop_map(|(runtimes, map, request)| Case { runtimes, map, request })
}

struct Fx {
    t: TreeBuilder,
    sources: Sources,
    home: PathBuf,
    cancel: AtomicBool,
    procs: FakeProcs,
    /// What each asset folder takes.
    sizes: Vec<u64>,
}

thread_local! {
    /// One fixture for every case: nothing in it changes, the fake
    /// `simctl` deletes nothing.
    static FX: Fx = Fx::new();
}

impl Fx {
    /// Runtime `k` has its own asset folder, of `k + 1` times 16 KiB.
    fn new() -> Self {
        let t = TreeBuilder::new();
        t.dir("home");
        for k in 0..MAX_RUNTIMES {
            t.file(&format!("{ASSETS}/r{k}.asset/AssetData/runtime.dmg"), (k + 1) * 16 * 1024);
        }
        let sources = Sources::under(&t.path("sys"));
        let home = t.path("home").canonicalize().unwrap();
        let sizes = (0..MAX_RUNTIMES).map(|k| reference_alloc(&t.path(&format!("{ASSETS}/r{k}.asset")))).collect();
        Self { t, sources, home, cancel: AtomicBool::new(false), procs: FakeProcs::new(vec![]), sizes }
    }

    fn ctx<'a>(&'a self, commands: &'a FakeCommands) -> FreeContext<'a> {
        FreeContext {
            timeout: Duration::from_secs(5),
            poll: Duration::from_millis(1),
            ..FreeContext::new(commands, &self.sources, &self.home, &[], &self.procs, &self.cancel)
        }
    }

    fn asset(&self, k: usize) -> PathBuf {
        self.t.path(&format!("{ASSETS}/r{k}.asset"))
    }

    /// `simctl runtime list -j` with the runtimes of `case` but `without`,
    /// each ready under AssetsV2.
    fn list(&self, case: &Case, without: Option<usize>) -> String {
        let entries: Vec<(usize, Value)> = case
            .runtimes
            .iter()
            .enumerate()
            .filter(|(k, _)| Some(*k) != without)
            .map(|(k, r)| {
                let data = self.asset(k).join("AssetData");
                let entry = json!({
                    "identifier": r.identifier, "runtimeIdentifier": r.runtime_identifier,
                    "version": r.version, "build": r.build, "sizeBytes": 7_000_000_000_u64,
                    "deletable": true, "state": "Ready", "path": data, "parentMountPath": data,
                });
                (k, entry)
            })
            .collect();
        if !case.map {
            return Value::Array(entries.into_iter().map(|(_, e)| e).collect()).to_string();
        }
        // A key is unique, the identifier inside it need not be.
        let mut map = serde_json::Map::new();
        for (k, entry) in entries {
            let id = entry["identifier"].as_str().unwrap().to_owned();
            let key = if map.contains_key(&id) { format!("{id}#{k}") } else { id };
            map.insert(key, entry);
        }
        Value::Object(map).to_string()
    }

    /// Developer tools ready, no device booted, the list answered in turn.
    fn simctl(&self, lists: &[String]) -> FakeCommands {
        let answers: Vec<(bool, &str)> = lists.iter().map(|l| (true, l.as_str())).collect();
        FakeCommands::default()
            .with("xcode-select", true, "/Applications/Xcode.app/Contents/Developer\n")
            .with("xcodebuild", true, "")
            .with("simctl", true, "")
            .answers_in_turn("simctl", "runtime list -j", &answers)
            .answer("simctl", "list devices -j", true, r#"{"devices": {}}"#)
    }
}

fn one(identifier: &str) -> ActionList {
    ActionList {
        schema_version: FREE_SCHEMA_VERSION,
        actions: vec![ListedAction::new(ActionId::SimctlRuntimeDelete, Some(identifier.to_owned()))],
    }
}

fn deletes(commands: &FakeCommands) -> Vec<String> {
    commands.calls.borrow().iter().filter(|c| c.starts_with("simctl runtime delete")).cloned().collect()
}

/// The report took runtime `k`: what it measured is `k`'s asset folder.
fn took(fx: &Fx, report: &ActionReport, k: usize) -> Result<(), TestCaseError> {
    prop_assert_eq!(report.bytes_before, Some(fx.sizes[k]), "not runtime {}: {:?}", k, report);
    Ok(())
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn a_runtime_identifier_takes_exactly_the_runtime_it_names_or_is_refused(case in cases()) {
        if common::unsupported("a_runtime_identifier_takes_exactly_the_runtime_it_names_or_is_refused") || common::lacks("a_runtime_identifier_takes_exactly_the_runtime_it_names_or_is_refused", Capability::WholeDisk) {
            return Ok(());
        }
        FX.with(|fx| check(fx, &case))?;
    }
}

/// The dry run, the delete and the list of what may run take exactly the
/// runtime `case.request` names, or refuse it.
fn check(fx: &Fx, case: &Case) -> Result<(), TestCaseError> {
    let same: Vec<usize> = (0..case.runtimes.len()).filter(|&k| case.runtimes[k].identifier == case.request).collect();
    let listed = fx.list(case, None);

    let dry = fx.simctl(std::slice::from_ref(&listed));
    let dry_report = free::dry_run(&fx.ctx(&dry), &one(&case.request)).unwrap().remove(0);

    let after = fx.list(case, same.first().copied());
    let real = fx.simctl(&[listed.clone(), after]);
    let mut journal = MemoryAreaJournal::default();
    let real_report = free::run(&fx.ctx(&real), &one(&case.request), &mut journal).unwrap().remove(0);

    match same[..] {
        [k] => {
            prop_assert_eq!(&dry_report.result, &ActionResult::WouldRun);
            took(fx, &dry_report, k)?;
            prop_assert_eq!(deletes(&dry), [format!("simctl runtime delete {} --dry-run", case.request)]);
            // The fake deletes nothing: the runtime leaves the list, its folder stays.
            prop_assert_eq!(&real_report.result, &ActionResult::RemovedNotFreed);
            took(fx, &real_report, k)?;
            prop_assert_eq!(deletes(&real), [format!("simctl runtime delete {}", case.request)]);
        }
        _ => {
            let refused = ActionResult::Refused { reason: Refusal::UnknownRuntime };
            prop_assert_eq!(&dry_report.result, &refused, "{} runtimes of that id", same.len());
            prop_assert_eq!(&real_report.result, &refused);
            prop_assert!(deletes(&dry).is_empty() && deletes(&real).is_empty(), "{:?}", real.calls.borrow());
        }
    }
    prop_assert_eq!(dry_report.bytes_before.is_some(), same.len() == 1);

    // What may run now: each identifier one runtime has, once.
    let offered = fx.simctl(std::slice::from_ref(&listed));
    let mut available: Vec<String> = free::available(&fx.ctx(&offered))
        .into_iter()
        .filter(|a| a.action == ActionId::SimctlRuntimeDelete)
        .filter_map(|a| a.identifier)
        .collect();
    available.sort();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &case.runtimes {
        *counts.entry(r.identifier.as_str()).or_default() += 1;
    }
    let unique: Vec<String> = counts.into_iter().filter(|(_, n)| *n == 1).map(|(id, _)| id.to_owned()).collect();
    prop_assert_eq!(available, unique);
    Ok(())
}
