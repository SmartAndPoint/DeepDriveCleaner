//! `ddc disk free` against fixture trees (story "Whole disk: free space with
//! the owning tool"): the system's folders live under a fixture root, and
//! `simctl` and `brew` are fakes. Nothing real is deleted or cleaned.

mod common;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use common::TreeBuilder;
use common::fakes::FakeCommands;
use ddc_core::disk::{ActionId, DISK_SCHEMA_VERSION, Sources};
use ddc_core::env::Env;
use ddc_core::exec::{JOURNAL_SCHEMA_VERSION, JournalKind, MemoryAreaJournal};
use ddc_core::free::{
    self, ActionList, ActionListError, ActionReport, ActionResult, FreeContext, FreeReport, ListedAction, Refusal,
    ReportEdit,
};
use ddc_core::model::SCHEMA_VERSION;
use ddc_core::report::{self, ScanOptions};
use ddc_core::scan::{Progress, WalkOptions};
use ddc_core::store;
use serde_json::{Value, json};

const ASSETS: &str = "sys/System/Library/AssetsV2/com_apple_MobileAsset_iOSSimulatorRuntime";
const DATA: &str = "sys/System/Volumes/Data";
const RID: &str = "com.apple.CoreSimulator.SimRuntime.iOS-27-1";

struct Fx {
    t: TreeBuilder,
    sources: Sources,
    home: PathBuf,
    cancel: AtomicBool,
}

impl Fx {
    fn new() -> Self {
        let t = TreeBuilder::new();
        t.dir("home");
        t.dir("data");
        t.file(&format!("{ASSETS}/a.asset/AssetData/runtime.dmg"), 256 * 1024);
        // `/opt` is the firmlink of `/System/Volumes/Data/opt`.
        t.file(&format!("{DATA}/opt/homebrew/Cellar/wget/1.0/bin/wget"), 64 * 1024);
        t.file(&format!("{DATA}/opt/homebrew/Cellar/wget/1.1/bin/wget"), 64 * 1024);
        t.symlink("System/Volumes/Data/opt", "sys/opt");
        t.file("home/Library/Caches/Homebrew/downloads/old.tar.gz", 128 * 1024);
        let sources = Sources::under(&t.path("sys"));
        let home = t.path("home").canonicalize().unwrap();
        Self { t, sources, home, cancel: AtomicBool::new(false) }
    }

    fn env(&self) -> Env {
        let now = SystemTime::now() + Duration::from_secs(3_600);
        Env::with_paths(&self.home, &self.t.path("data"), &self.t.path("data/logs"), now).unwrap()
    }

    fn ctx<'a>(&'a self, commands: &'a FakeCommands, skips: &'a [PathBuf]) -> FreeContext<'a> {
        FreeContext {
            timeout: Duration::from_secs(5),
            poll: Duration::from_millis(1),
            ..FreeContext::new(commands, &self.sources, &self.home, skips, &self.cancel)
        }
    }

    /// Where a fake tool's deletions go: outside the fixture's measured folders.
    fn holding(&self) -> PathBuf {
        self.t.dir("gone")
    }

    fn asset(&self) -> PathBuf {
        self.t.path(&format!("{ASSETS}/a.asset"))
    }

    /// The runtime `A`, under AssetsV2, ready, with `edit` applied.
    fn runtime(&self, edit: impl FnOnce(&mut Value)) -> Value {
        let data = self.asset().join("AssetData");
        let mut r = json!({
            "identifier": "A",
            "runtimeIdentifier": RID,
            "version": "27.1", "build": "24A94425", "sizeBytes": 7_000_000_000_u64,
            "deletable": true, "state": "Ready",
            "path": data, "parentMountPath": data,
        });
        edit(&mut r);
        r
    }

    fn list(&self, runtimes: &[Value]) -> String {
        let map: serde_json::Map<String, Value> =
            runtimes.iter().map(|r| (r["identifier"].as_str().unwrap().to_owned(), r.clone())).collect();
        Value::Object(map).to_string()
    }

    fn brew_cache(&self) -> PathBuf {
        self.home.join("Library/Caches/Homebrew")
    }

    /// `brew` of the area, answering as the fixture's Homebrew.
    fn brew(&self, commands: FakeCommands) -> FakeCommands {
        let prefix = self.t.path("sys/opt/homebrew");
        commands
            .with("brew", true, "")
            .answer("brew", "--prefix", true, &format!("{}\n", prefix.display()))
            .answer("brew", "--cellar", true, &format!("{}\n", prefix.join("Cellar").display()))
            .answer("brew", "--cache", true, &format!("{}\n", self.brew_cache().display()))
            .answer("brew", "cleanup -n", true, BREW_DRY_RUN)
    }
}

const BREW_DRY_RUN: &str = "Would remove: /opt/homebrew/Cellar/wget/1.0 (3 files, 64KB)\n\
==> This operation would free approximately 64KB of disk space.\n";

const DEVICES_NONE_BOOTED: &str = r#"{"devices": {"com.apple.CoreSimulator.SimRuntime.iOS-27-1": [
  {"udid": "D1", "name": "iPhone A", "state": "Shutdown"}]}}"#;

/// Developer tools ready, `A` listed, then gone after the delete.
fn simctl(fx: &Fx, before: &[Value]) -> FakeCommands {
    let listed = fx.list(before);
    FakeCommands::default()
        .with("xcode-select", true, "/Applications/Xcode.app/Contents/Developer\n")
        .with("xcodebuild", true, "")
        .with("simctl", true, "")
        .answers_in_turn("simctl", "runtime list -j", &[(true, &listed), (true, &listed), (true, "{}")])
        .answer("simctl", "list devices -j", true, DEVICES_NONE_BOOTED)
}

/// Moves `path` into `holding`, out of every measured folder, as a tool's
/// delete would empty it.
fn gone(path: impl AsRef<Path>, holding: &Path) {
    let n = COUNT.fetch_add(1, Ordering::SeqCst);
    fs::rename(path, holding.join(n.to_string())).unwrap();
}

static COUNT: AtomicUsize = AtomicUsize::new(0);

fn one(action: ActionId, identifier: Option<&str>) -> ActionList {
    ActionList { schema_version: 1, actions: vec![ListedAction { action, identifier: identifier.map(str::to_owned) }] }
}

fn run(fx: &Fx, commands: &FakeCommands, list: &ActionList) -> (Vec<ActionReport>, MemoryAreaJournal) {
    let mut journal = MemoryAreaJournal::default();
    let reports = free::run(&fx.ctx(commands, &[]), list, &mut journal).unwrap();
    (reports, journal)
}

fn deletes(commands: &FakeCommands) -> Vec<String> {
    commands.calls.borrow().iter().filter(|c| c.contains("delete")).cloned().collect()
}

#[test]
fn a_runtime_is_deleted_with_coresimulators_simctl_alone_and_its_bytes_are_measured() {
    let fx = Fx::new();
    let (asset, holding) = (fx.asset(), fx.holding());
    let commands = FakeCommands {
        on_call: Some(Box::new(move |call| {
            if call == "simctl runtime delete A" {
                gone(&asset, &holding);
            }
        })),
        ..simctl(&fx, &[fx.runtime(|_| {})])
    };

    let (reports, journal) = run(&fx, &commands, &one(ActionId::SimctlRuntimeDelete, Some("A")));

    let r = &reports[0];
    assert_eq!(r.result, ActionResult::Freed);
    assert_eq!(deletes(&commands), ["simctl runtime delete A"]);
    let i = commands.calls.borrow().iter().position(|c| c == "simctl runtime delete A").unwrap();
    assert_eq!(commands.exes.borrow()[i], fx.sources.simctl);
    assert!(commands.envs.borrow()[i].contains(&"DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer".to_owned()));
    assert!(!commands.calls.borrow().iter().any(|c| c.contains("keep-asset") || c.starts_with("xcrun")));
    assert!(r.bytes_before.unwrap() >= 256 * 1024, "{r:?}");
    assert_eq!(r.bytes_after, Some(0));
    assert_eq!(r.area, "simulator-runtimes");
    assert_eq!(
        r.terminal_command,
        format!(
            "DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer {} runtime delete A",
            fx.sources.simctl.display()
        )
    );

    let line = serde_json::to_value(&journal.0[0]).unwrap();
    assert_eq!(journal.0.len(), 1);
    assert_eq!(line["schema_version"], JOURNAL_SCHEMA_VERSION);
    assert_eq!(line["kind"], "area-action");
    assert_eq!(line["action"], "simctl-runtime-delete");
    assert_eq!(line["identifier"], "A");
    assert_eq!(line["command"], r.terminal_command.as_str());
    assert_eq!(line["result"]["status"], "freed");
    assert!(line.get("path").is_none() && line.get("rule").is_none());
}

#[test]
fn the_journal_has_its_own_version_and_the_scan_reports_is_unchanged() {
    assert_eq!(JOURNAL_SCHEMA_VERSION, 3);
    assert_eq!(SCHEMA_VERSION, 1);
    assert_eq!(serde_json::to_value(JournalKind::Item).unwrap(), "item");
}

#[test]
fn every_runtime_the_probe_cannot_vouch_for_is_refused_before_any_delete() {
    let fx = Fx::new();
    let outside = fx.t.dir("elsewhere/x.asset/AssetData");
    let evil = fx.t.dir("sys/System/Library/AssetsV2Evil/x.asset/AssetData");
    let dotdot = PathBuf::from(format!("{}/../../../../elsewhere/x.asset/AssetData", fx.asset().display()));
    let booted = r#"{"devices": {"com.apple.CoreSimulator.SimRuntime.iOS-27-1": [
      {"udid": "D1", "name": "iPhone A", "state": "Booted"}]}}"#;
    let booting = r#"{"devices": {"com.apple.CoreSimulator.SimRuntime.iOS-27-1": [
      {"udid": "D1", "name": "iPhone A", "state": "Shutdown"}, {"udid": "D2", "name": "iPhone B", "state": "Booting"}]}}"#;
    type Case = (&'static str, &'static str, Box<dyn Fn(&mut Value)>, Option<&'static str>, Refusal);
    let cases: Vec<Case> = vec![
        ("unknown id", "Z", Box::new(|_| {}), None, Refusal::UnknownRuntime),
        ("all", "all", Box::new(|_| {}), None, Refusal::UnknownRuntime),
        ("a build", "24A94425", Box::new(|_| {}), None, Refusal::UnknownRuntime),
        ("a flag", "--unusable", Box::new(|_| {}), None, Refusal::UnknownRuntime),
        ("booted", "A", Box::new(|_| {}), Some(booted), Refusal::Booted),
        ("booting", "A", Box::new(|_| {}), Some(booting), Refusal::Booted),
        ("not deletable", "A", Box::new(|r| r["deletable"] = json!(false)), None, Refusal::NotDeletable),
        ("deleting", "A", Box::new(|r| r["state"] = json!("Deleting")), None, Refusal::Deleting),
        (
            "no runtimeIdentifier",
            "A",
            Box::new(|r| {
                r.as_object_mut().unwrap().remove("runtimeIdentifier");
            }),
            None,
            Refusal::NoRuntimeIdentifier,
        ),
        (
            "outside AssetsV2",
            "A",
            Box::new(move |r| r["parentMountPath"] = json!(outside)),
            None,
            Refusal::NotUnderAssets,
        ),
        ("AssetsV2Evil", "A", Box::new(move |r| r["parentMountPath"] = json!(evil)), None, Refusal::NotUnderAssets),
        ("dot dot", "A", Box::new(move |r| r["parentMountPath"] = json!(dotdot)), None, Refusal::NotUnderAssets),
        (
            "no asset folder",
            "A",
            Box::new(|r| {
                r.as_object_mut().unwrap().remove("path");
            }),
            None,
            Refusal::NoAssetFolder,
        ),
        ("devices unknown", "A", Box::new(|_| {}), Some("not json"), Refusal::DevicesUnknown),
    ];
    for (name, id, edit, devices, reason) in cases {
        let mut commands = simctl(&fx, &[fx.runtime(edit)]);
        if let Some(devices) = devices {
            commands = commands.answer("simctl", "list devices -j", true, devices);
        }
        let (reports, journal) = run(&fx, &commands, &one(ActionId::SimctlRuntimeDelete, Some(id)));
        assert_eq!(reports[0].result, ActionResult::Refused { reason }, "{name}");
        assert!(deletes(&commands).is_empty(), "{name}: {:?}", commands.calls.borrow());
        assert_eq!(journal.0.len(), 1, "{name}: a refusal is journaled");
        // The dry run refuses the same way and runs nothing either.
        let dry = free::dry_run(&fx.ctx(&commands, &[]), &one(ActionId::SimctlRuntimeDelete, Some(id)));
        assert_eq!(dry[0].result, ActionResult::Refused { reason }, "{name} (dry run)");
        assert!(deletes(&commands).is_empty(), "{name} (dry run)");
    }
}

#[test]
fn without_ready_developer_tools_nothing_is_listed_or_deleted() {
    let fx = Fx::new();
    let none = FakeCommands::default().with("xcode-select", false, "").with("simctl", true, "");
    let (reports, _) = run(&fx, &none, &one(ActionId::SimctlRuntimeDelete, Some("A")));
    assert_eq!(reports[0].result, ActionResult::Refused { reason: Refusal::NoDeveloperTools });
    let pending = simctl(&fx, &[fx.runtime(|_| {})]).with("xcodebuild", false, "");
    let (reports, _) = run(&fx, &pending, &one(ActionId::SimctlRuntimeDelete, Some("A")));
    assert_eq!(reports[0].result, ActionResult::Refused { reason: Refusal::FirstLaunchPending });
    for commands in [&none, &pending] {
        assert!(!commands.calls.borrow().iter().any(|c| c.starts_with("simctl")), "{:?}", commands.calls.borrow());
    }
}

#[test]
fn a_runtime_that_left_the_list_while_its_folder_kept_its_size_is_removed_not_freed() {
    let fx = Fx::new();
    let commands = simctl(&fx, &[fx.runtime(|_| {})]);
    let (reports, _) = run(&fx, &commands, &one(ActionId::SimctlRuntimeDelete, Some("A")));
    let r = &reports[0];
    assert_eq!(r.result, ActionResult::RemovedNotFreed);
    assert_eq!(r.bytes_before, r.bytes_after);
    assert!(r.bytes_before.unwrap() > 0);
}

#[test]
fn a_runtime_still_listed_at_the_deadline_is_still_deleting() {
    let fx = Fx::new();
    let listed = fx.list(&[fx.runtime(|_| {})]);
    let commands = simctl(&fx, &[]).answers_in_turn("simctl", "runtime list -j", &[(true, &listed)]);
    let mut journal = MemoryAreaJournal::default();
    let ctx =
        FreeContext { timeout: Duration::from_millis(80), poll: Duration::from_millis(10), ..fx.ctx(&commands, &[]) };
    let reports = free::run(&ctx, &one(ActionId::SimctlRuntimeDelete, Some("A")), &mut journal).unwrap();
    assert_eq!(reports[0].result, ActionResult::StillDeleting);
    assert_eq!(reports[0].bytes_after, None);
    assert_eq!(serde_json::to_value(&journal.0[0]).unwrap()["result"]["status"], "still-deleting");
}

#[test]
fn a_stop_during_the_wait_ends_it_as_still_deleting_and_journals_it() {
    let fx = Fx::new();
    let listed = fx.list(&[fx.runtime(|_| {})]);
    let stop = Arc::new(AtomicBool::new(false));
    let set = Arc::clone(&stop);
    let commands = FakeCommands {
        on_call: Some(Box::new(move |call| {
            if call == "simctl runtime delete A" {
                set.store(true, Ordering::SeqCst);
            }
        })),
        ..simctl(&fx, &[]).answers_in_turn("simctl", "runtime list -j", &[(true, &listed)])
    };
    let skips: [PathBuf; 0] = [];
    let ctx = FreeContext { cancel: &stop, poll: Duration::from_secs(60), ..fx.ctx(&commands, &skips) };
    let list = ActionList {
        schema_version: 1,
        actions: vec![
            ListedAction { action: ActionId::SimctlRuntimeDelete, identifier: Some("A".into()) },
            ListedAction { action: ActionId::BrewCleanup, identifier: None },
        ],
    };
    let mut journal = MemoryAreaJournal::default();
    let started = std::time::Instant::now();
    let reports = free::run(&ctx, &list, &mut journal).unwrap();
    assert!(started.elapsed() < Duration::from_secs(30));
    assert_eq!(reports[0].result, ActionResult::StillDeleting);
    assert_eq!(reports[1].result, ActionResult::NotRun);
    assert_eq!(journal.0.len(), 2);
    assert!(!commands.calls.borrow().iter().any(|c| c.starts_with("brew")));
}

/// A stop while `simctl runtime delete` itself runs: the list, not the
/// killed tool's exit, says what happened.
#[test]
fn a_stop_during_the_delete_call_reads_the_list_once() {
    let fx = Fx::new();
    let ready = fx.list(&[fx.runtime(|_| {})]);
    let deleting = fx.list(&[fx.runtime(|r| r["state"] = json!("Deleting"))]);
    for (after, expected) in
        [(&deleting, ActionResult::StillDeleting), (&ready, ActionResult::Failed { needs_admin: false })]
    {
        let stop = Arc::new(AtomicBool::new(false));
        let set = Arc::clone(&stop);
        let commands =
            FakeCommands {
                on_call: Some(Box::new(move |call| {
                    if call == "simctl runtime delete A" {
                        set.store(true, Ordering::SeqCst);
                    }
                })),
                ..simctl(&fx, &[])
                    .answers_in_turn("simctl", "runtime list -j", &[(true, &ready), (true, after)])
                    .answer("simctl", "runtime delete A", false, "")
            };
        let skips: [PathBuf; 0] = [];
        let ctx = FreeContext { cancel: &stop, ..fx.ctx(&commands, &skips) };
        let mut journal = MemoryAreaJournal::default();
        let reports = free::run(&ctx, &one(ActionId::SimctlRuntimeDelete, Some("A")), &mut journal).unwrap();
        assert_eq!(reports[0].result, expected);
        if expected != ActionResult::StillDeleting {
            assert!(reports[0].output.starts_with(free::STOPPED), "{}", reports[0].output);
        }
        assert_eq!(journal.0.len(), 1);
    }
}

#[test]
fn brew_cleanup_runs_alone_from_the_areas_brew_with_no_autoremove() {
    let fx = Fx::new();
    let old = fx.t.path(&format!("{DATA}/opt/homebrew/Cellar/wget/1.0"));
    let (cache, holding) = (fx.brew_cache().join("downloads"), fx.holding());
    let commands = FakeCommands {
        on_call: Some(Box::new(move |call| {
            if call == "brew cleanup" {
                gone(&old, &holding);
                gone(&cache, &holding);
            }
        })),
        ..fx.brew(FakeCommands::default())
    };

    let (reports, journal) = run(&fx, &commands, &one(ActionId::BrewCleanup, None));

    let r = &reports[0];
    assert_eq!(r.result, ActionResult::Freed);
    let calls = commands.calls.borrow();
    let runs: Vec<usize> =
        calls.iter().enumerate().filter(|(_, c)| c.starts_with("brew cleanup")).map(|(i, _)| i).collect();
    assert_eq!(runs.len(), 1);
    assert_eq!(calls[runs[0]], "brew cleanup");
    assert_eq!(commands.exes.borrow()[runs[0]], fx.sources.brew);
    let env = &commands.envs.borrow()[runs[0]];
    for var in ["HOMEBREW_NO_AUTOREMOVE=1", "HOMEBREW_NO_ENV_HINTS=1", "NO_COLOR=1"] {
        assert!(env.contains(&var.to_owned()), "{env:?}");
    }
    assert!(r.bytes_after.unwrap() + 192 * 1024 <= r.bytes_before.unwrap(), "{r:?}");
    assert_eq!(
        r.terminal_command,
        format!("HOMEBREW_NO_AUTOREMOVE=1 HOMEBREW_NO_ENV_HINTS=1 NO_COLOR=1 {} cleanup", fx.sources.brew.display())
    );
    assert_eq!(journal.0.len(), 1);
}

#[test]
fn the_dry_run_runs_each_tools_own_dry_run_and_changes_nothing() {
    let fx = Fx::new();
    let commands = fx.brew(simctl(&fx, &[fx.runtime(|_| {})])).answer(
        "simctl",
        "runtime delete A --dry-run",
        true,
        "Would delete A\n",
    );
    let list = ActionList {
        schema_version: 1,
        actions: vec![
            ListedAction { action: ActionId::SimctlRuntimeDelete, identifier: Some("A".into()) },
            ListedAction { action: ActionId::BrewCleanup, identifier: None },
        ],
    };

    let reports = free::dry_run(&fx.ctx(&commands, &[]), &list);

    assert_eq!(reports[0].result, ActionResult::WouldRun);
    let preview = reports[0].preview.as_ref().unwrap();
    assert_eq!(preview.lines, ["Would delete A"]);
    assert_eq!(preview.estimate_bytes, reports[0].bytes_before);
    assert_eq!(reports[0].command.last().unwrap(), "A", "the report names the command that would run");
    assert_eq!(reports[1].result, ActionResult::WouldRun);
    assert_eq!(reports[1].preview.as_ref().unwrap().estimate_bytes, Some(64 * 1024));
    let calls = commands.calls.borrow();
    assert!(calls.contains(&"simctl runtime delete A --dry-run".to_owned()));
    assert!(!calls.contains(&"simctl runtime delete A".to_owned()));
    assert!(!calls.contains(&"brew cleanup".to_owned()));
    let i = calls.iter().position(|c| c == "brew cleanup -n").unwrap();
    assert!(commands.envs.borrow()[i].contains(&"HOMEBREW_NO_AUTOREMOVE=1".to_owned()));
    assert!(fx.asset().exists());
}

#[test]
fn a_homebrew_elsewhere_is_refused() {
    let fx = Fx::new();
    let intel = fx.t.dir("sys/usr/local");
    fx.t.dir("sys/usr/local/Cellar");
    let other_cache = fx.t.dir("home/other-cache");
    let prefix = fx.t.path("sys/opt/homebrew");
    let cases: Vec<(&str, &str, String, Refusal)> = vec![
        ("intel prefix", "--prefix", format!("{}\n", intel.display()), Refusal::PrefixMismatch),
        ("cellar outside", "--cellar", format!("{}\n", intel.join("Cellar").display()), Refusal::CellarOutside),
        ("cellar is the prefix", "--cellar", format!("{}\n", prefix.display()), Refusal::CellarOutside),
        ("cache elsewhere", "--cache", format!("{}\n", other_cache.display()), Refusal::CacheMismatch),
        ("relative", "--prefix", "opt/homebrew\n".to_owned(), Refusal::BrewMissing),
    ];
    for (name, call, answer, reason) in cases {
        let commands = fx.brew(FakeCommands::default()).answer("brew", call, true, &answer);
        let (reports, _) = run(&fx, &commands, &one(ActionId::BrewCleanup, None));
        assert_eq!(reports[0].result, ActionResult::Refused { reason }, "{name}");
        assert!(!commands.ran("brew cleanup"), "{name}");
    }
    let missing = FakeCommands::default();
    let (reports, _) = run(&fx, &missing, &one(ActionId::BrewCleanup, None));
    assert_eq!(reports[0].result, ActionResult::Refused { reason: Refusal::BrewMissing });
}

#[test]
fn a_missing_cache_folder_is_compared_as_a_path() {
    let fx = Fx::new();
    gone(fx.brew_cache(), &fx.holding());
    let commands = fx.brew(FakeCommands::default());
    let (reports, _) = run(&fx, &commands, &one(ActionId::BrewCleanup, None));
    assert_eq!(reports[0].result, ActionResult::Freed);

    let elsewhere = fx.brew(FakeCommands::default()).answer("brew", "--cache", true, "/Users/example/cache\n");
    let (reports, _) = run(&fx, &elsewhere, &one(ActionId::BrewCleanup, None));
    assert_eq!(reports[0].result, ActionResult::Refused { reason: Refusal::CacheMismatch });
}

#[test]
fn a_failing_tool_carries_its_terminal_command_and_names_an_administrator_only_on_a_permission_error() {
    let fx = Fx::new();
    let denied = fx.brew(FakeCommands::default()).answer("brew", "cleanup", false, "").stderr_on(
        "brew",
        "cleanup",
        "Error: Permission denied @ apply2files - /opt/homebrew/Cellar/x",
    );
    let (reports, _) = run(&fx, &denied, &one(ActionId::BrewCleanup, None));
    assert_eq!(reports[0].result, ActionResult::Failed { needs_admin: true });
    assert!(reports[0].output.contains("Permission denied"));
    assert!(reports[0].terminal_command.ends_with("brew cleanup"));

    let other = fx.brew(FakeCommands::default()).answer("brew", "cleanup", false, "Error: something else");
    let (reports, _) = run(&fx, &other, &one(ActionId::BrewCleanup, None));
    assert_eq!(reports[0].result, ActionResult::Failed { needs_admin: false });

    let timeout = fx.brew(FakeCommands::default()).error_on("brew", "cleanup", io::ErrorKind::TimedOut);
    let (reports, _) = run(&fx, &timeout, &one(ActionId::BrewCleanup, None));
    assert_eq!(reports[0].result, ActionResult::Failed { needs_admin: false });
    assert!(reports[0].output.contains("timed out"), "{}", reports[0].output);
}

#[test]
fn a_skipped_folder_is_not_measured_and_the_action_still_runs() {
    let fx = Fx::new();
    let commands = fx.brew(FakeCommands::default());
    let skips = [fx.brew_cache()];
    let mut journal = MemoryAreaJournal::default();
    let reports = free::run(&fx.ctx(&commands, &skips), &one(ActionId::BrewCleanup, None), &mut journal).unwrap();
    assert_eq!(reports[0].result, ActionResult::Freed);
    assert_eq!((reports[0].bytes_before, reports[0].bytes_after), (None, None));
}

#[test]
fn every_malformed_list_is_refused_whole() {
    let cases: Vec<(&str, ActionListError)> = vec![
        ("{", ActionListError::Syntax(String::new())),
        (r#"{"schema_version": 2, "actions": [{"action": "brew-cleanup"}]}"#, ActionListError::Version(2)),
        (r#"{"schema_version": 1, "actions": []}"#, ActionListError::Empty),
        (
            r#"{"schema_version": 1, "actions": [{"action": "brew-cleanup"}, {"action": "brew-cleanup"}]}"#,
            ActionListError::Duplicate("brew-cleanup".into()),
        ),
        (
            r#"{"schema_version": 1, "actions": [{"action": "simctl-runtime-delete", "identifier": "A"}, {"action": "simctl-runtime-delete", "identifier": "A"}]}"#,
            ActionListError::Duplicate("simctl-runtime-delete A".into()),
        ),
        (
            r#"{"schema_version": 1, "actions": [{"action": "brew-cleanup", "identifier": "x"}]}"#,
            ActionListError::IdentifierOnBrew,
        ),
        (
            r#"{"schema_version": 1, "actions": [{"action": "simctl-runtime-delete"}]}"#,
            ActionListError::MissingIdentifier,
        ),
        (r#"{"schema_version": 1, "actions": [{"action": "rm-rf"}]}"#, ActionListError::Syntax(String::new())),
        (
            r#"{"schema_version": 1, "actions": [{"action": "brew-cleanup", "force": true}]}"#,
            ActionListError::Syntax(String::new()),
        ),
        (r#"{"schema_version": 1, "actions": [], "yes": true}"#, ActionListError::Syntax(String::new())),
    ];
    for (json, want) in cases {
        let got = ActionList::parse(json.as_bytes()).unwrap_err();
        match want {
            ActionListError::Syntax(_) => assert!(matches!(got, ActionListError::Syntax(_)), "{json}: {got:?}"),
            want => assert_eq!(got, want, "{json}"),
        }
    }
    let two = r#"{"schema_version": 1, "actions": [{"action": "simctl-runtime-delete", "identifier": "A"},
        {"action": "simctl-runtime-delete", "identifier": "B"}, {"action": "brew-cleanup"}]}"#;
    assert_eq!(ActionList::parse(two.as_bytes()).unwrap().actions.len(), 3);
}

#[test]
fn available_offers_the_runtimes_the_probe_passes_and_homebrew() {
    let fx = Fx::new();
    let refused = fx.runtime(|r| {
        r["identifier"] = json!("B");
        r["deletable"] = json!(false);
    });
    let commands = fx.brew(simctl(&fx, &[fx.runtime(|_| {}), refused]));
    let offered = free::available(&fx.ctx(&commands, &[]));
    assert_eq!(
        offered,
        [
            ListedAction { action: ActionId::SimctlRuntimeDelete, identifier: Some("A".into()) },
            ListedAction { action: ActionId::BrewCleanup, identifier: None },
        ]
    );
    assert!(deletes(&commands).is_empty());
}

/// A saved disk report with both areas and runtimes `A` and `B`.
fn last_disk(fx: &Fx) -> Value {
    let area = |id: &str| {
        json!({"id": id, "paths": [], "title": id, "explanation": "", "frees": "", "bytes": 100,
               "shared_bytes": 0, "files": 1, "dirs": 1, "unreadable": 0, "partial": false})
    };
    let runtime = |id: &str, size: u64| json!({"identifier": id, "size_bytes": size, "devices": [], "booted": false});
    json!({
        "schema_version": DISK_SCHEMA_VERSION, "ddc_version": "0", "generated_at": "2026-10-28T00:00:00Z",
        "duration_ms": 1, "cancelled": false,
        "home": {"path": fx.home, "source": "walk", "as_of": "2026-10-28T00:00:00Z", "bytes": 1, "unreadable": 0, "partial": false},
        "areas": [area("simulator-runtimes"), area("homebrew"), area("applications")],
        "simulators": {"status": "listed", "runtimes": [runtime("A", 7), runtime("B", 8)], "listed_bytes": 15},
        "snapshots": {}, "walk": {"files": 0, "dirs": 0, "bytes": 0},
        "not_counted": {"counted_bytes": 1, "inconsistent": false, "holds": [], "unreadable": 0}
    })
}

fn report_of(action: ActionId, identifier: Option<&str>, result: ActionResult) -> ActionReport {
    ActionReport {
        action,
        area: action.area().to_owned(),
        identifier: identifier.map(str::to_owned),
        command: vec![],
        env: vec![],
        terminal_command: String::new(),
        result,
        bytes_before: None,
        bytes_after: None,
        output: String::new(),
        preview: None,
    }
}

#[test]
fn last_disk_marks_the_areas_that_ran_stale_and_drops_a_runtime_that_left() {
    let fx = Fx::new();
    let env = fx.env();
    let dir = env.data_dir().to_path_buf();
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("last-disk.json"), last_disk(&fx).to_string()).unwrap();

    let refused = [report_of(ActionId::BrewCleanup, None, ActionResult::Refused { reason: Refusal::BrewMissing })];
    assert_eq!(free::edit_last_disk(&env, &refused), ReportEdit::Unchanged);

    let reports = [
        report_of(ActionId::SimctlRuntimeDelete, Some("A"), ActionResult::Freed),
        report_of(ActionId::BrewCleanup, None, ActionResult::Failed { needs_admin: false }),
    ];
    assert_eq!(free::edit_last_disk(&env, &reports), ReportEdit::Edited);
    let disk = store::read_last_disk(&env).unwrap().unwrap();
    let stale: Vec<(&str, bool)> = disk.areas.iter().map(|a| (a.id.as_str(), a.stale)).collect();
    assert_eq!(stale, [("simulator-runtimes", true), ("homebrew", true), ("applications", false)]);
    let sims = disk.simulators.unwrap();
    assert_eq!(sims.runtimes.iter().map(|r| r.identifier.as_str()).collect::<Vec<_>>(), ["B"]);
    assert_eq!(sims.listed_bytes, 8);
    assert_eq!(disk.generated_at, "2026-10-28T00:00:00Z");
    let saved: Value = serde_json::from_slice(&fs::read(dir.join("last-disk.json")).unwrap()).unwrap();
    assert!(saved["areas"][2].get("stale").is_none(), "absent when false");

    // Still deleting: the area is stale, the runtime stays listed.
    fs::write(dir.join("last-disk.json"), last_disk(&fx).to_string()).unwrap();
    let waiting = [report_of(ActionId::SimctlRuntimeDelete, Some("A"), ActionResult::StillDeleting)];
    assert_eq!(free::edit_last_disk(&env, &waiting), ReportEdit::Edited);
    let disk = store::read_last_disk(&env).unwrap().unwrap();
    assert!(disk.areas[0].stale && !disk.areas[1].stale);
    assert_eq!(disk.simulators.unwrap().runtimes.len(), 2);

    // Of another version, or absent: left alone.
    let mut other = last_disk(&fx);
    other["schema_version"] = json!(DISK_SCHEMA_VERSION + 1);
    fs::write(dir.join("last-disk.json"), other.to_string()).unwrap();
    assert_eq!(free::edit_last_disk(&env, &reports), ReportEdit::Skipped);
    gone(dir.join("last-disk.json"), &fx.holding());
    assert_eq!(free::edit_last_disk(&env, &reports), ReportEdit::Skipped);
}

#[test]
fn after_brew_cleanup_the_homebrew_cache_finding_is_measured_again() {
    let fx = Fx::new();
    let env = fx.env();
    let opts =
        ScanOptions { walk: WalkOptions { threads: Some(2), ..WalkOptions::default() }, ..ScanOptions::default() };
    let scan = report::scan(&env, &opts, &Progress::default()).unwrap();
    let cache = fx.brew_cache();
    let before = scan.findings.iter().find(|f| f.rule == "cache.homebrew").expect("a cache.homebrew finding").bytes;
    store::write_last_scan(&env, &scan).unwrap();
    gone(cache.join("downloads"), &fx.holding());

    let ran = [report_of(ActionId::BrewCleanup, None, ActionResult::Freed)];
    let runtime = [report_of(ActionId::SimctlRuntimeDelete, Some("A"), ActionResult::Freed)];
    assert_eq!(free::edit_last_scan(&env, &opts, &runtime), ReportEdit::Unchanged);
    assert_eq!(free::edit_last_scan(&env, &opts, &ran), ReportEdit::Edited);

    let after = store::read_last_scan(&env).unwrap().unwrap();
    let finding = after.findings.iter().find(|f| f.rule == "cache.homebrew");
    assert!(finding.is_none_or(|f| f.bytes < before), "{finding:?}");

    // A report of other roots is not that report.
    let other = ScanOptions { roots: vec![fx.t.dir("home/Projects")], ..opts.clone() };
    assert_eq!(free::edit_last_scan(&env, &other, &ran), ReportEdit::Skipped);
}

#[test]
#[allow(clippy::disallowed_methods)] // DDC_UPDATE_SCHEMA regenerates the committed file
fn committed_free_schema_is_current() {
    let mut generator = schemars::SchemaGenerator::default();
    // The list the app sends is part of the same contract.
    generator.subschema_for::<ActionList>();
    let schema = serde_json::to_string_pretty(&generator.into_root_schema_for::<FreeReport>()).unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/disk-free.schema.json");
    if std::env::var_os("DDC_UPDATE_SCHEMA").is_some() {
        fs::write(&path, &schema).unwrap();
    }
    let committed = fs::read_to_string(&path).unwrap_or_default();
    assert!(committed == schema, "docs/disk-free.schema.json is stale: run DDC_UPDATE_SCHEMA=1 cargo test");
}
