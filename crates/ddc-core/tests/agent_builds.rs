//! Story "Coding agents on Linux: ddc disk agents" (DDC-010), PR C2: a coding
//! agent's old builds are the picked entries of its `versions` folder, to the
//! Trash only, and a build still needed is locked (safety ADR, amendment of
//! 2026-11-02).

mod common;

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use common::TreeBuilder;
use common::fakes::{FakeCommands, FakeProcs};
use ddc_core::agents::builds::{BuildBy, BuildsCause, Host};
use ddc_core::class::RiskClass;
use ddc_core::env::Env;
use ddc_core::exec::preflight::{self, Prediction, PreflightLine, PreflightPorts};
use ddc_core::exec::{
    self, Consent, ExecOptions, ExecReport, MemoryJournal, Outcome, Ports, ProcessTable, SkipReason, TrashDone,
    TrashError, TrashTarget, Trasher,
};
use ddc_core::model::{OriginKind, ScanReport};
use ddc_core::plan::{ActionKind, ListedItem, ListedPlan, PlanOptions, build_plan, plan_listed_with};
use ddc_core::platform::procs::ProcInfo;
use ddc_core::report::contents::EntryIndex;
use ddc_core::report::recheck;
use ddc_core::report::volumes::SystemStatFs;
use ddc_core::report::{self, Processes, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::rules::entries::EntryLock;
use ddc_core::scan::{Progress, StdReader, WalkOptions};

const HOUR: Duration = Duration::from_secs(3_600);
const VERSIONS: &str = ".local/share/claude/versions";
const CLAUDE_BUILDS: &str = "agent.claude-code-builds";
const CURSOR_VERSIONS: &str = ".local/share/cursor-agent/versions";
const CURSOR_BUILDS: &str = "agent.cursor-agent-builds";

struct Home {
    t: TreeBuilder,
    data: tempfile::TempDir,
    /// An hour after the files were written: no build is `recent`.
    now: SystemTime,
}

impl Home {
    fn new() -> Self {
        Self { t: TreeBuilder::new(), data: tempfile::tempdir().unwrap(), now: SystemTime::now() + HOUR }
    }

    fn env(&self) -> Env {
        Env::with_paths(self.t.root(), self.data.path(), &self.data.path().join("logs"), self.now).unwrap()
    }

    fn catalog(&self) -> Catalog {
        Catalog::builtin(self.t.root())
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.t.path(rel)
    }

    /// Claude Code's builds, the launcher pointing to `current`.
    fn claude(&self, builds: &[&str], current: &str) {
        for b in builds {
            self.t.file(&format!("{VERSIONS}/{b}"), 50_000);
        }
        self.t.symlink(self.path(&format!("{VERSIONS}/{current}")), ".local/bin/claude");
    }

    fn scan_with(&self, procs: FakeProcs) -> (ScanReport, EntryIndex) {
        let opts = ScanOptions {
            walk: WalkOptions { threads: Some(4), ..Default::default() },
            processes: Processes::fixed(procs),
            ..Default::default()
        };
        report::scan_with_entries(&self.env(), &opts, &Progress::default(), &SystemStatFs).unwrap()
    }

    /// The clean's pruned re-check of `items`, then its plan: what
    /// `clean --items` does after a scan of the same roots.
    fn pruned(&self, items: &[ListedItem]) -> ListedPlan {
        let opts = ScanOptions {
            walk: WalkOptions { threads: Some(4), ..Default::default() },
            processes: Processes::fixed(FakeProcs::new(Vec::new())),
            ..Default::default()
        };
        let catalog = self.catalog();
        let (r, index) =
            recheck::pruned(&self.env(), &opts, &catalog, items, &Progress::default(), &SystemStatFs, None).unwrap();
        plan_listed_with(&r, &index, &catalog, items, &FakeProcs::new(Vec::new()), &[])
    }

    fn entry(&self, rel: &str, rule: &str) -> ListedItem {
        let path = self.path(rel);
        let (dev, ino) = common::id(&path);
        ListedItem {
            path,
            dev,
            ino,
            rule: rule.to_owned(),
            class: RiskClass::UserData,
            action: ActionKind::Trash,
            permanent: false,
        }
    }

    fn plan(&self, items: &[ListedItem], procs: FakeProcs) -> ListedPlan {
        let (r, index) = self.scan_with(procs);
        let table = FakeProcs::new(Vec::new());
        plan_listed_with(&r, &index, &self.catalog(), items, &table, &[])
    }

    fn run(&self, plan: &ListedPlan, trash: &dyn Trasher, procs: &dyn ProcessTable) -> ExecReport {
        let mut journal = MemoryJournal::default();
        exec::execute(
            &self.env(),
            &self.catalog(),
            &plan.plan,
            &Consent::Listed,
            Ports { commands: &FakeCommands::default(), trash, procs, journal: &mut journal },
            &ExecOptions { cancel: &AtomicBool::new(false), ..ExecOptions::default() },
        )
        .unwrap()
    }

    fn predict(&self, items: &[ListedItem], procs: &dyn ProcessTable) -> Vec<(Option<u64>, Prediction)> {
        let mut predicted = Vec::new();
        let ports = PreflightPorts {
            commands: &FakeCommands::default(),
            procs,
            reader: &StdReader,
            trash: &common::fakes::AnyTrash,
        };
        preflight::preflight_skipping(&self.env(), &self.catalog(), &[], &[], &[], items, &ports, &mut |line| {
            if let PreflightLine::Item(i) = line {
                predicted.push((i.bytes, i.prediction.clone()));
            }
            Ok(())
        })
        .unwrap();
        predicted
    }

    fn trash(&self) -> RecordingTrash {
        RecordingTrash { dir: self.data.path().join("Trash"), moved: Mutex::default() }
    }
}

struct RecordingTrash {
    dir: PathBuf,
    moved: Mutex<Vec<PathBuf>>,
}

impl Trasher for RecordingTrash {
    fn trash(&self, target: &TrashTarget<'_>) -> Result<TrashDone, TrashError> {
        std::fs::create_dir_all(&self.dir)?;
        std::fs::rename(target.path, self.dir.join(target.name))?;
        self.moved.lock().unwrap().push(target.path.to_path_buf());
        Ok(TrashDone::Moved(None))
    }
}

/// A process of the user running `exe`, resolved.
fn running(exe: PathBuf) -> ProcInfo {
    ProcInfo {
        pid: 77,
        name: "claude".to_owned(),
        exe: Some(exe.clone()),
        resolved_exe: Some(exe),
        ..ProcInfo::default()
    }
}

fn lock_of(r: &ScanReport, path: &std::path::Path) -> Option<EntryLock> {
    let entries = &r.findings.iter().find(|f| f.rule == CLAUDE_BUILDS).unwrap().contents.as_ref().unwrap().entries;
    entries.iter().find(|e| e.path == path).unwrap_or_else(|| panic!("{path:?} not listed: {entries:?}")).lock.clone()
}

/// The folder is a `review` finding with no method and the origin kind
/// `builds`; each build is an entry, locked when still needed; an
/// installer's temporary file is no entry; `--yes` never takes a build.
#[test]
fn old_builds_are_entries_and_the_ones_still_needed_are_locked() {
    if common::unsupported("old_builds_are_entries_and_the_ones_still_needed_are_locked") {
        return;
    }
    let h = Home::new();
    h.claude(&["2.1.100", "2.1.110", "2.1.120", "2.1.130"], "2.1.120");
    h.t.file(&format!("{VERSIONS}/2.1.130.tmp.123.456"), 1_000);
    let procs = FakeProcs::new(vec![running(h.path(&format!("{VERSIONS}/2.1.110")))]);
    let (r, _) = h.scan_with(procs);

    let folder = r.findings.iter().find(|f| f.rule == CLAUDE_BUILDS).expect("the builds folder");
    assert_eq!(folder.origin.map(|o| o.kind), Some(OriginKind::Builds));
    assert_eq!((folder.class, folder.selected), (RiskClass::Review, false));
    let contents = folder.contents.as_ref().unwrap();
    assert_eq!(contents.entries.len(), 4, "{:?}", contents.entries);
    assert_eq!(contents.rest_count, 1, "the temporary file");
    let at = |b: &str| lock_of(&r, &h.path(&format!("{VERSIONS}/{b}")));
    assert_eq!(at("2.1.100"), None);
    assert_eq!(at("2.1.110"), Some(EntryLock::BuildInUse { by: BuildBy::Process }));
    assert_eq!(at("2.1.120"), Some(EntryLock::BuildInUse { by: BuildBy::Launcher }));
    assert_eq!(at("2.1.130"), Some(EntryLock::BuildInUse { by: BuildBy::Newest }));

    let plan = build_plan(&r, &h.catalog(), &PlanOptions::default());
    assert!(!plan.items.iter().any(|i| i.finding.path.starts_with(h.path(VERSIONS))), "never by --yes");
}

/// A free build moves to the Trash through its folder, a file entry; one
/// still needed is refused, by the dry run and by the clean alike.
#[test]
fn a_picked_free_build_moves_to_the_trash_and_a_needed_one_is_refused() {
    if common::unsupported("a_picked_free_build_moves_to_the_trash_and_a_needed_one_is_refused") {
        return;
    }
    let h = Home::new();
    h.claude(&["2.1.100", "2.1.130"], "2.1.130");
    let items = [
        h.entry(&format!("{VERSIONS}/2.1.100"), CLAUDE_BUILDS),
        h.entry(&format!("{VERSIONS}/2.1.130"), CLAUDE_BUILDS),
    ];
    let none = FakeProcs::new(Vec::new());
    let predicted = h.predict(&items, &none);
    assert_eq!(predicted[0].1, Prediction::WouldTrash);
    assert!(predicted[0].0.is_some_and(|b| b >= 50_000), "{predicted:?}");
    assert_eq!(
        predicted[1].1,
        Prediction::WouldSkip(SkipReason::EntryLocked { lock: EntryLock::BuildInUse { by: BuildBy::Launcher } })
    );

    let plan = h.plan(&items, FakeProcs::new(Vec::new()));
    let trash = h.trash();
    let run = h.run(&plan, &trash, &none);
    let outcome = |p: PathBuf| run.items.iter().find(|i| i.path == p).map(|i| i.outcome.clone());
    assert_eq!(outcome(h.path(&format!("{VERSIONS}/2.1.100"))), Some(Outcome::Trashed));
    assert_eq!(*trash.moved.lock().unwrap(), vec![h.path(&format!("{VERSIONS}/2.1.100"))]);
    assert!(h.path(&format!("{VERSIONS}/2.1.130")).exists(), "the launcher's build stays");
}

/// The guard decides again before the move: a launcher pointed at the
/// picked build after the plan, or a process started from it, keeps it.
#[test]
fn a_build_that_came_into_use_after_the_plan_is_refused() {
    if common::unsupported("a_build_that_came_into_use_after_the_plan_is_refused") {
        return;
    }
    let h = Home::new();
    h.claude(&["2.1.100", "2.1.110", "2.1.130"], "2.1.130");
    let old = h.path(&format!("{VERSIONS}/2.1.100"));
    let items = [h.entry(&format!("{VERSIONS}/2.1.100"), CLAUDE_BUILDS)];
    let plan = h.plan(&items, FakeProcs::new(Vec::new()));
    assert!(plan.refused.is_empty(), "{:?}", plan.refused);

    let started = FakeProcs::new(vec![running(old.clone())]);
    let run = h.run(&plan, &h.trash(), &started);
    assert!(
        matches!(
            &run.items[0].outcome,
            Outcome::Skipped(SkipReason::EntryLocked { lock: EntryLock::BuildInUse { .. } })
        ),
        "{:?}",
        run.items[0].outcome
    );
    assert!(old.exists());

    std::fs::rename(h.path(".local/bin/claude"), h.path(".local/bin/claude.old")).unwrap();
    h.t.symlink(&old, ".local/bin/claude");
    let run = h.run(&plan, &h.trash(), &FakeProcs::new(Vec::new()));
    assert_eq!(
        run.items[0].outcome,
        Outcome::Skipped(SkipReason::EntryLocked { lock: EntryLock::BuildInUse { by: BuildBy::Launcher } })
    );
    assert!(old.exists());
}

/// What cannot be told locks every build: another user running the
/// harness, a hidden process table, a network home.
#[test]
fn what_the_machine_cannot_tell_locks_every_build() {
    if common::unsupported("what_the_machine_cannot_tell_locks_every_build") {
        return;
    }
    let h = Home::new();
    h.claude(&["2.1.100", "2.1.130"], "2.1.130");
    let local = Host { others: Vec::new(), hidden: false, home_fs: Some("apfs".to_owned()) };
    for (host, cause) in [
        (Host { others: vec!["claude".to_owned()], ..local.clone() }, BuildsCause::OtherUserProcess),
        (Host { hidden: true, ..local.clone() }, BuildsCause::HiddenProcessTable),
        (Host { home_fs: Some("nfs".to_owned()), ..local.clone() }, BuildsCause::NetworkHome),
    ] {
        let (r, _) = h.scan_with(FakeProcs { host: Some(host), ..FakeProcs::new(Vec::new()) });
        assert_eq!(lock_of(&r, &h.path(&format!("{VERSIONS}/2.1.100"))), Some(EntryLock::BuildsUnknown { cause }));
    }
}

/// Cursor's agent keeps a folder per build: a free one moves whole.
#[test]
fn a_free_cursor_agent_build_folder_moves_to_the_trash() {
    if common::unsupported("a_free_cursor_agent_build_folder_moves_to_the_trash") {
        return;
    }
    let h = Home::new();
    for b in ["2026.9.30-aa11", "2026.10.22-bb22"] {
        h.t.file(&format!("{CURSOR_VERSIONS}/{b}/cursor-agent"), 40_000);
    }
    h.t.symlink(h.path(&format!("{CURSOR_VERSIONS}/2026.10.22-bb22/cursor-agent")), ".local/bin/cursor-agent");
    let items = [h.entry(&format!("{CURSOR_VERSIONS}/2026.9.30-aa11"), CURSOR_BUILDS)];
    let none = FakeProcs::new(Vec::new());
    assert_eq!(h.predict(&items, &none)[0].1, Prediction::WouldTrash);
    let plan = h.plan(&items, FakeProcs::new(Vec::new()));
    let trash = h.trash();
    let run = h.run(&plan, &trash, &none);
    assert_eq!(run.items[0].outcome, Outcome::Trashed);
    assert!(!h.path(&format!("{CURSOR_VERSIONS}/2026.9.30-aa11")).exists());
    assert!(h.path(&format!("{CURSOR_VERSIONS}/2026.10.22-bb22")).exists());
}

/// The pruned re-check of `clean --items` reads the picked build's
/// siblings too: an old build picked alone is not taken for the newest.
#[test]
fn the_pruned_recheck_reads_the_other_builds_and_frees_an_old_one() {
    if common::unsupported("the_pruned_recheck_reads_the_other_builds_and_frees_an_old_one") {
        return;
    }
    let h = Home::new();
    for b in ["2026.9.30-aa11", "2026.10.22-bb22"] {
        h.t.file(&format!("{CURSOR_VERSIONS}/{b}/cursor-agent"), 40_000);
    }
    h.t.symlink(h.path(&format!("{CURSOR_VERSIONS}/2026.10.22-bb22/cursor-agent")), ".local/bin/cursor-agent");
    let items = [h.entry(&format!("{CURSOR_VERSIONS}/2026.9.30-aa11"), CURSOR_BUILDS)];
    let plan = h.pruned(&items);
    assert!(plan.refused.is_empty(), "not `build-in-use: newest`: {:?}", plan.refused);
    let none = FakeProcs::new(Vec::new());
    let run = h.run(&plan, &h.trash(), &none);
    assert_eq!(run.items[0].outcome, Outcome::Trashed);
}
