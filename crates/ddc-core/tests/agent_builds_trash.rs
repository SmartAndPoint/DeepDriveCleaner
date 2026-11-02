//! Story "Coding agents on Linux: ddc disk agents" (DDC-010): a picked old
//! Claude Code build, a file, moves through ddc's own Trash on Linux into the
//! home's freedesktop Trash, with its `.trashinfo`. The system Trash reads
//! `HOME`: the test runs its clean in a child process of this binary whose
//! `HOME` is the test's tree, which a marker file there tells it.
#![cfg(target_os = "linux")]

mod common;

use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use common::TreeBuilder;
use common::fakes::{FakeCommands, FakeProcs};
use ddc_core::class::RiskClass;
use ddc_core::env::Env;
use ddc_core::exec::{self, Consent, ExecOptions, MemoryJournal, Outcome, Ports, SystemTrash};
use ddc_core::plan::{ActionKind, ListedItem, plan_listed_with};
use ddc_core::report::volumes::SystemStatFs;
use ddc_core::report::{self, Processes, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::scan::{Progress, WalkOptions};

const TEST: &str = "a_picked_old_build_moves_into_the_homes_freedesktop_trash";
const MARKER: &str = ".ddc-agent-builds-trash-test";
const VERSIONS: &str = ".local/share/claude/versions";

#[test]
fn a_picked_old_build_moves_into_the_homes_freedesktop_trash() {
    if common::unsupported("a_picked_old_build_moves_into_the_homes_freedesktop_trash") {
        return;
    }
    // In the child: `HOME` is the tree the parent made.
    if let Some(home) = ddc_core::platform::folders::home().filter(|h| h.join(MARKER).exists()) {
        clean(&home);
        return;
    }
    let t = TreeBuilder::new();
    t.write(MARKER, "");
    for b in ["2.1.100", "2.1.130"] {
        t.file(&format!("{VERSIONS}/{b}"), 30_000);
    }
    t.symlink(t.path(&format!("{VERSIONS}/2.1.130")), ".local/bin/claude");
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", TEST, "--nocapture"])
        .env("HOME", t.root())
        .env_remove("XDG_DATA_HOME")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let old = t.path(&format!("{VERSIONS}/2.1.100"));
    assert!(!old.exists());
    let trash = t.path(".local/share/Trash");
    assert!(trash.join("files/2.1.100").is_file(), "in the home's Trash");
    let record = std::fs::read_to_string(trash.join("info/2.1.100.trashinfo")).unwrap();
    assert!(record.starts_with("[Trash Info]\n") && record.contains(&format!("Path={}", old.display())), "{record}");
    assert!(t.path(&format!("{VERSIONS}/2.1.130")).exists(), "the launcher's build stays");
}

/// The scan, the plan of the picked old build and the clean with the system Trash.
fn clean(home: &Path) {
    let data = tempfile::tempdir().unwrap();
    // An hour on: the builds the parent made are not `recent`.
    let now = SystemTime::now() + Duration::from_secs(3_600);
    let env = Env::with_paths(home, data.path(), &data.path().join("logs"), now).unwrap();
    let opts = ScanOptions {
        walk: WalkOptions { threads: Some(2), ..Default::default() },
        processes: Processes::fixed(FakeProcs::new(Vec::new())),
        ..Default::default()
    };
    let (r, index) = report::scan_with_entries(&env, &opts, &Progress::default(), &SystemStatFs).unwrap();
    let old = home.join(VERSIONS).join("2.1.100");
    let (dev, ino) = common::id(&old);
    let item = ListedItem {
        path: old,
        dev,
        ino,
        rule: "agent.claude-code-builds".to_owned(),
        class: RiskClass::UserData,
        action: ActionKind::Trash,
        permanent: false,
    };
    let catalog = Catalog::builtin(home);
    let none = FakeProcs::new(Vec::new());
    let plan = plan_listed_with(&r, &index, &catalog, &[item], &none, &[]);
    assert!(plan.refused.is_empty(), "{:?}", plan.refused);
    let mut journal = MemoryJournal::default();
    let run = exec::execute(
        &env,
        &catalog,
        &plan.plan,
        &Consent::Listed,
        Ports { commands: &FakeCommands::default(), trash: &SystemTrash, procs: &none, journal: &mut journal },
        &ExecOptions { cancel: &AtomicBool::new(false), ..ExecOptions::default() },
    )
    .unwrap();
    assert_eq!(run.items[0].outcome, Outcome::Trashed);
}
