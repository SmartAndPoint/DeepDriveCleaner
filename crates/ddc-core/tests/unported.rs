//! A build with `SUPPORTED` false (ADR "Platform seam: one platform module
//! per OS with declared capabilities", §8): built only by
//! `make unported-check`, with the `simulate-unported` feature, so the
//! macOS test host stands in for a platform that is not supported. Every
//! library entry point that reads or changes the disk refuses before its
//! first system call: the temporary folders are as they were, and the
//! fakes recorded no call.
#![cfg(feature = "simulate-unported")]

mod common;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use common::TreeBuilder;
use common::fakes::{FakeCommands, FakeProcs, FakeTrash};
use ddc_core::class::RiskClass;
use ddc_core::disk::{self, ActionId, DiskOptions, DiskPorts, Sources};
use ddc_core::env::Env;
use ddc_core::exec::preflight::{self, PreflightPorts};
use ddc_core::exec::{self, Consent, ExecError, ExecOptions, MemoryAreaJournal, MemoryJournal, Ports};
use ddc_core::free::{self, ActionList, FREE_SCHEMA_VERSION, FreeContext, ListedAction};
use ddc_core::model::Finding;
use ddc_core::plan::{Action, ActionKind, ListedItem, Plan, PlanItem};
use ddc_core::platform::{self, PLATFORM, Refusal, Unsupported};
use ddc_core::report::volumes::SystemStatFs;
use ddc_core::report::{self, ScanError, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::scan::{self, NullVisitor, Progress, StdReader, WalkOptions, WalkSetupError};

const REFUSED: Unsupported = Unsupported { platform: PLATFORM, simulated: true };

/// A home with an old `node_modules`, and an empty data folder.
struct Fx {
    t: TreeBuilder,
    data: tempfile::TempDir,
}

impl Fx {
    fn new() -> Self {
        let t = TreeBuilder::new();
        t.write("p/package.json", "{}");
        t.file("p/node_modules/dep/index.js", 4_000);
        Self { t, data: tempfile::tempdir().unwrap() }
    }

    fn env(&self) -> Env {
        Env::with_paths(self.t.root(), self.data.path(), &self.data.path().join("logs"), SystemTime::now()).unwrap()
    }

    fn node_modules(&self) -> PathBuf {
        self.t.path("p/node_modules")
    }

    /// Every file under the home with its length, and the data folder's
    /// entries: what a refused call must leave as it was.
    fn state(&self) -> (Vec<(PathBuf, u64)>, Vec<PathBuf>) {
        fn files(dir: &Path, out: &mut Vec<(PathBuf, u64)>) {
            for entry in fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let meta = entry.metadata().unwrap();
                if meta.is_dir() {
                    files(&entry.path(), out);
                }
                out.push((entry.path(), if meta.is_dir() { 0 } else { meta.len() }));
            }
        }
        let mut home = Vec::new();
        files(self.t.root(), &mut home);
        home.sort();
        let data = fs::read_dir(self.data.path()).unwrap().map(|e| e.unwrap().path()).collect();
        (home, data)
    }

    fn listed(&self) -> ListedItem {
        let (dev, ino) = common::id(&self.node_modules());
        ListedItem {
            path: self.node_modules(),
            dev,
            ino,
            rule: "node.node_modules".into(),
            class: RiskClass::Rebuildable,
            action: ActionKind::Trash,
            permanent: false,
        }
    }

    /// One selected item to the Trash, as a plan of the scan would hold it.
    fn plan(&self) -> Plan {
        let path = self.node_modules();
        let (dev, ino) = common::id(&path);
        let (parent_dev, parent_ino) = common::id(path.parent().unwrap());
        let finding: Finding = serde_json::from_value(serde_json::json!({
            "bytes": 4096, "category": "project-artifact", "class": "rebuildable", "dev": dev,
            "dirs": 2, "files": 1, "hint": "", "incomplete": false, "ino": ino, "notes": [],
            "parent_dev": parent_dev, "parent_ino": parent_ino, "path": path, "removal": "trash",
            "rule": "node.node_modules", "rule_class": "rebuildable", "selected": true, "shared_bytes": 0
        }))
        .unwrap();
        Plan {
            roots: vec![self.t.root().to_path_buf()],
            items: vec![PlanItem { finding, action: Action::Trash, default_selected: true, selected: true }],
        }
    }
}

#[test]
fn the_build_says_it_is_not_supported() {
    const { assert!(!platform::SUPPORTED) };
    assert_eq!(platform::require_supported(), Err(REFUSED));
    assert_eq!(REFUSED.to_string(), "this build was made with simulate-unported and refuses every scan and clean");
}

#[test]
fn a_scan_is_refused_before_it_reads_the_home() {
    let fx = Fx::new();
    let before = fx.state();
    let err = report::scan(&fx.env(), &ScanOptions::default(), &Progress::default()).unwrap_err();
    assert!(matches!(err, ScanError::Unsupported(u) if u == REFUSED), "{err:?}");
    assert_eq!(platform::refusal_in(&err), Some(Refusal::Unsupported(REFUSED)));
    assert_eq!(fx.state(), before);
}

#[test]
fn a_walk_is_refused() {
    let fx = Fx::new();
    let progress = Progress::default();
    let err = scan::walk(&[fx.t.root().to_path_buf()], &NullVisitor, &WalkOptions::default(), &progress).unwrap_err();
    assert!(matches!(err, WalkSetupError::Unsupported(u) if u == REFUSED), "{err:?}");
    assert_eq!(progress.snapshot().files, 0, "nothing was read");
}

#[test]
fn the_disk_report_is_refused_before_any_tool_runs() {
    let fx = Fx::new();
    let before = fx.state();
    let commands = FakeCommands::default();
    let ports = DiskPorts { commands: &commands, statfs: &SystemStatFs };
    let cancel = AtomicBool::new(false);
    let sources = Sources::under(fx.t.root());
    let r = disk::measure(&fx.env(), &sources, &DiskOptions::default(), &ports, &Progress::default(), &cancel);
    assert_eq!(r.unwrap_err(), REFUSED);
    assert!(commands.calls.borrow().is_empty(), "{:?}", commands.calls.borrow());
    assert_eq!(fx.state(), before);
}

#[test]
fn a_clean_is_refused_before_it_moves_anything() {
    let fx = Fx::new();
    let before = fx.state();
    let commands = FakeCommands::default();
    let trash = FakeTrash(fx.data.path().join("FakeTrash"));
    let mut journal = MemoryJournal::default();
    let err = exec::execute(
        &fx.env(),
        &Catalog::builtin(fx.t.root()),
        &fx.plan(),
        &Consent::AssumeYes { classes: vec![RiskClass::Rebuildable] },
        Ports { commands: &commands, trash: &trash, procs: &FakeProcs::new(vec![]), journal: &mut journal },
        &ExecOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, ExecError::Unsupported(u) if u == REFUSED), "{err:?}");
    assert_eq!(platform::refusal_in(&err), Some(Refusal::Unsupported(REFUSED)));
    assert!(journal.0.is_empty() && commands.calls.borrow().is_empty());
    assert!(fx.node_modules().exists());
    assert_eq!(fx.state(), before);
}

#[test]
fn a_dry_run_is_refused_before_it_checks_an_item() {
    let fx = Fx::new();
    let before = fx.state();
    let commands = FakeCommands::default();
    let ports = PreflightPorts { commands: &commands, procs: &FakeProcs::new(vec![]), reader: &StdReader };
    let mut lines = 0;
    let err =
        preflight::preflight(&fx.env(), &Catalog::builtin(fx.t.root()), &[], &[], &[fx.listed()], &ports, &mut |_| {
            lines += 1;
            Ok(())
        })
        .unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    assert_eq!(platform::refusal_in(&err), Some(Refusal::Unsupported(REFUSED)));
    assert_eq!(lines, 0);
    assert!(commands.calls.borrow().is_empty());
    assert_eq!(fx.state(), before);
}

#[test]
fn a_tool_action_is_refused_before_the_tool_runs() {
    let fx = Fx::new();
    let before = fx.state();
    let commands = FakeCommands::default().with("brew", true, "");
    let sources = Sources::under(fx.t.root());
    let cancel = AtomicBool::new(false);
    let procs = FakeProcs::new(vec![]);
    let skips: [PathBuf; 0] = [];
    let ctx = FreeContext::new(&commands, &sources, fx.t.root(), &skips, &procs, &cancel);
    let list = ActionList {
        schema_version: FREE_SCHEMA_VERSION,
        actions: vec![ListedAction::new(ActionId::BrewCleanup, None)],
    };
    let mut journal = MemoryAreaJournal::default();
    let err = free::run(&ctx, &list, &mut journal).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    assert_eq!(platform::refusal_in(&err), Some(Refusal::Unsupported(REFUSED)));
    assert!(journal.0.is_empty() && commands.calls.borrow().is_empty());
    assert_eq!(fx.state(), before);
}
