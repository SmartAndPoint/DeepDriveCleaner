//! Safety of `ddc clean` at the executor level (story "Safe cleanup: ddc
//! clean", ADR "Cleanup safety model", §5). Real filesystem in
//! temp dirs; commands, Trash and (where noted) processes are fakes.

mod common;

use std::cell::RefCell;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use common::TreeBuilder;
use common::fakes::{FakeCommands, FakeProcs, FakeTrash, app_process};
use ddc_core::class::RiskClass;
use ddc_core::env::Env;
use ddc_core::exec::{
    self, Consent, ExecError, ExecEvents, ExecOptions, ExecPhase, ExecProgress, ExecReport, ExecSnapshot, Journal,
    JournalEntry, MemoryJournal, OpenVnode, Outcome, Ports, ProcStart, ProcessTable, SkipReason, SystemProcesses,
};
use ddc_core::model::ScanReport;
use ddc_core::plan::{Action, Plan, PlanItem, PlanOptions, build_plan};
use ddc_core::platform::procs::ProcInfo;
use ddc_core::report::{self, ScanOptions};
use ddc_core::rules::{Catalog, RULES, owns};
use ddc_core::scan::{Progress, WalkOptions};

const DAY: Duration = Duration::from_secs(86_400);

/// Docker Desktop's readiness probe, as the fake runner keys it.
const DOCKER_VERSION: &str = "--context desktop-linux version --format {{.Server.Version}}";

struct Fx {
    t: TreeBuilder,
    data: tempfile::TempDir,
    now: SystemTime,
}

impl Fx {
    fn new() -> Self {
        Self { t: TreeBuilder::new(), data: tempfile::tempdir().unwrap(), now: SystemTime::now() }
    }

    fn env(&self) -> Env {
        Env::with_paths(self.t.root(), self.data.path(), &self.data.path().join("logs"), self.now).unwrap()
    }

    fn old(&self, rel: &str) {
        File::options().write(true).open(self.t.path(rel)).unwrap().set_modified(self.now - DAY * 60).unwrap();
    }

    /// Idle Node project (selected by default).
    fn node_project(&self, rel: &str) -> PathBuf {
        let t = &self.t;
        t.write(&format!("{rel}/package.json"), "{}");
        t.write(&format!("{rel}/package-lock.json"), "{}");
        t.file(&format!("{rel}/node_modules/dep/index.js"), 20_000);
        self.old(&format!("{rel}/package.json"));
        self.old(&format!("{rel}/package-lock.json"));
        t.path(&format!("{rel}/node_modules"))
    }

    /// Idle uv project with a venv (selected by default).
    fn venv_project(&self, rel: &str) -> PathBuf {
        let t = &self.t;
        t.write(&format!("{rel}/uv.lock"), "");
        t.write(&format!("{rel}/.venv/pyvenv.cfg"), "home = /usr/bin");
        t.file(&format!("{rel}/.venv/lib/x.py"), 10_000);
        self.old(&format!("{rel}/uv.lock"));
        t.path(&format!("{rel}/.venv"))
    }

    fn scan(&self) -> ScanReport {
        let opts = ScanOptions { walk: WalkOptions { threads: Some(2), ..Default::default() }, ..Default::default() };
        report::scan(&self.env(), &opts, &Progress::default()).unwrap()
    }

    fn plan(&self) -> Plan {
        build_plan(&self.scan(), &Catalog::builtin(self.t.root()), &PlanOptions::default())
    }

    fn run(
        &self,
        plan: &Plan,
        consent: Consent,
        commands: &FakeCommands,
        procs: &dyn ProcessTable,
    ) -> (ExecReport, MemoryJournal) {
        let mut journal = MemoryJournal::default();
        let report = self.run_with(plan, consent, commands, procs, &mut journal, &ExecOptions::default());
        (report, journal)
    }

    fn run_with(
        &self,
        plan: &Plan,
        consent: Consent,
        commands: &FakeCommands,
        procs: &dyn ProcessTable,
        journal: &mut dyn Journal,
        opts: &ExecOptions<'_>,
    ) -> ExecReport {
        let trash = FakeTrash(self.data.path().join("FakeTrash"));
        let env = self.env();
        exec::execute(
            &env,
            &Catalog::builtin(self.t.root()),
            plan,
            &consent,
            Ports { commands, trash: &trash, procs, journal },
            opts,
        )
        .unwrap()
    }

    /// Docker Desktop's folder with its disk image, as the scan finds it.
    fn docker_desktop(&self) -> PathBuf {
        self.t.file("Library/Containers/com.docker.docker/Data/vms/0/Docker.raw", 50_000);
        self.t.path("Library/Containers/com.docker.docker")
    }

    fn run_yes(&self, plan: &Plan) -> (ExecReport, MemoryJournal) {
        let consent = Consent::AssumeYes { classes: vec![RiskClass::Safe, RiskClass::Rebuildable] };
        self.run(plan, consent, &FakeCommands::default(), &FakeProcs::new(vec![]))
    }
}

fn outcome<'a>(r: &'a ExecReport, path: &Path) -> &'a Outcome {
    &r.items.iter().find(|i| i.path == path).unwrap_or_else(|| panic!("{path:?} not executed: {:#?}", r.items)).outcome
}

fn select_all(plan: &mut Plan) {
    plan.items.iter_mut().for_each(|i| i.selected = true);
}

fn yes() -> Consent {
    Consent::AssumeYes { classes: vec![RiskClass::Safe, RiskClass::Rebuildable] }
}

#[test]
fn deletes_selected_items_and_journals_each() {
    if common::unsupported("deletes_selected_items_and_journals_each") {
        return;
    }
    let fx = Fx::new();
    let nm = fx.node_project("p");
    let cache = fx.t.dir("q/__pycache__");
    fx.t.file("q/__pycache__/m.pyc", 1000);

    let plan = fx.plan();
    let (r, journal) = fx.run_yes(&plan);

    assert_eq!(outcome(&r, &nm), &Outcome::Deleted);
    assert_eq!(outcome(&r, &cache), &Outcome::Deleted);
    assert!(!nm.exists() && !cache.exists());
    assert!(fx.t.path("p/package.json").exists(), "only the finding is removed");
    assert_eq!(journal.0.len(), 2, "one journal line per action");
    let e = journal.0.iter().find(|e| e.path == nm).unwrap();
    // The journal's own version, not the scan report's (safety ADR, amendment of 2026-10-28).
    assert_eq!((e.rule.as_str(), e.method, e.schema_version), ("node.node_modules", "delete", 4));
    let line = serde_json::to_value(e).unwrap();
    assert_eq!(line["kind"], "item");
    for key in ["ts", "path", "dev", "ino", "rule", "class", "method", "bytes", "outcome"] {
        assert!(line.get(key).is_some(), "journal field {key}: {line}");
    }
}

#[test]
fn symlink_swapped_in_after_scan_is_not_followed() {
    if common::unsupported("symlink_swapped_in_after_scan_is_not_followed") {
        return;
    }
    let fx = Fx::new();
    let nm = fx.node_project("p");
    let victim = fx.t.file("precious/data.db", 5000);
    let plan = fx.plan();
    fs::rename(&nm, fx.t.path("p/moved")).unwrap();
    common::symlink(fx.t.path("precious"), &nm);

    let (r, journal) = fx.run_yes(&plan);

    assert_eq!(outcome(&r, &nm), &Outcome::Skipped(SkipReason::Symlink));
    assert!(victim.exists());
    assert_eq!(journal.0[0].outcome, Outcome::Skipped(SkipReason::Symlink), "reason is journaled");
}

#[test]
fn same_path_and_marker_but_another_directory_is_not_deleted() {
    if common::unsupported("same_path_and_marker_but_another_directory_is_not_deleted") {
        return;
    }
    let fx = Fx::new();
    let nm = fx.node_project("p");
    let plan = fx.plan();
    fs::rename(&nm, fx.t.path("p/old_modules")).unwrap();
    fx.t.file("p/node_modules/fresh/index.js", 100);

    let (r, _) = fx.run_yes(&plan);

    assert_eq!(outcome(&r, &nm), &Outcome::Skipped(SkipReason::ChangedSinceScan));
    assert!(fx.t.path("p/node_modules/fresh/index.js").exists());
}

#[test]
fn protected_directories_and_project_roots_are_refused_even_if_planned() {
    if common::unsupported("protected_directories_and_project_roots_are_refused_even_if_planned") {
        return;
    }
    let fx = Fx::new();
    let nm = fx.node_project("p");
    fx.t.file("Documents/cv.pdf", 100);
    fx.t.dir("Projects");
    let mut plan = fx.plan();
    let template = plan.items.iter().find(|i| i.finding.path == nm).unwrap().clone();
    let forged = |path: PathBuf| {
        let mut item = template.clone();
        item.finding.path = path;
        item
    };
    let home = fx.t.root().to_path_buf();
    let upper = home.join("DOCUMENTS");
    plan.items = vec![forged(home.clone()), forged(home.join("Documents")), forged(upper.clone())];
    // A finding that is itself a git work tree.
    fx.t.dir("p/node_modules/.git");
    plan.items.push(template.clone());
    plan.roots = vec![home.clone()];

    let (r, _) = fx.run_yes(&plan);

    assert_eq!(outcome(&r, &home), &Outcome::Skipped(SkipReason::ScanRoot));
    assert_eq!(outcome(&r, &home.join("Documents")), &Outcome::Skipped(SkipReason::Protected));
    assert_eq!(outcome(&r, &upper), &Outcome::Skipped(SkipReason::Protected), "case-insensitive APFS");
    // A git work tree no longer matches the rule (verify) or is refused as a project root.
    assert!(matches!(outcome(&r, &nm), Outcome::Skipped(SkipReason::RuleMismatch | SkipReason::ProjectRoot)));
    assert!(fx.t.path("Documents/cv.pdf").exists() && nm.exists());
}

#[test]
fn assume_yes_never_touches_review_and_rejects_review_class() {
    if common::unsupported("assume_yes_never_touches_review_and_rejects_review_class") {
        return;
    }
    let fx = Fx::new();
    let orphan = fx.t.dir("x/node_modules");
    fx.t.file("x/node_modules/a.js", 100);
    let venv = fx.venv_project("py");
    let mut plan = fx.plan();
    select_all(&mut plan);

    let (r, _) = fx.run(
        &plan,
        Consent::AssumeYes { classes: vec![RiskClass::Safe] },
        &FakeCommands::default(),
        &FakeProcs::new(vec![]),
    );

    assert_eq!(outcome(&r, &orphan), &Outcome::Skipped(SkipReason::NeedsConfirmation), "review never under --yes");
    assert_eq!(
        outcome(&r, &venv),
        &Outcome::Skipped(SkipReason::NeedsConfirmation),
        "rebuildable needs --class rebuildable"
    );
    assert!(orphan.exists() && venv.exists());

    let mut journal = MemoryJournal::default();
    let err = exec::execute(
        &fx.env(),
        &Catalog::builtin(fx.t.root()),
        &plan,
        &Consent::AssumeYes { classes: vec![RiskClass::Safe, RiskClass::Review] },
        Ports {
            commands: &FakeCommands::default(),
            trash: &FakeTrash(fx.t.path("T")),
            procs: &FakeProcs::new(vec![]),
            journal: &mut journal,
        },
        &ExecOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, ExecError::ReviewWithYes));
}

#[test]
fn interactive_review_goes_to_the_trash() {
    if common::unsupported("interactive_review_goes_to_the_trash") {
        return;
    }
    let fx = Fx::new();
    let orphan = fx.t.dir("x/node_modules");
    fx.t.file("x/node_modules/a.js", 100);
    let mut plan = fx.plan();
    select_all(&mut plan);

    let (r, _) = fx.run(&plan, Consent::Interactive, &FakeCommands::default(), &FakeProcs::new(vec![]));

    assert_eq!(outcome(&r, &orphan), &Outcome::Trashed);
    assert!(!orphan.exists() && fx.data.path().join("FakeTrash/node_modules/a.js").exists());
}

#[test]
fn native_only_rules_never_delete_or_trash() {
    if common::unsupported("native_only_rules_never_delete_or_trash") {
        return;
    }
    let fx = Fx::new();
    let docker = fx.t.dir("Library/Containers/com.docker.docker/Data/vms/0");
    fx.t.file("Library/Containers/com.docker.docker/Data/vms/0/Docker.raw", 50_000);
    let docker = docker.parent().unwrap().parent().unwrap().parent().unwrap().to_path_buf();
    let mut plan = fx.plan();
    select_all(&mut plan);

    let (missing, _) = fx.run(&plan, Consent::Interactive, &FakeCommands::default(), &FakeProcs::new(vec![]));
    assert!(matches!(outcome(&missing, &docker), Outcome::Skipped(SkipReason::NativeUnavailable { .. })));

    let failing = FakeCommands::default().with("docker", false, "").answer("docker", DOCKER_VERSION, true, "28.5.1\n");
    let (r, _) = fx.run(&plan, Consent::Interactive, &failing, &FakeProcs::new(vec![]));
    let Outcome::Native { commands } = outcome(&r, &docker) else { panic!("{:?}", outcome(&r, &docker)) };
    assert!(
        commands.iter().all(|c| !c.success
            && c.args.contains(&"prune".to_owned())
            && c.args.contains(&"desktop-linux".to_owned()))
    );
    assert!(
        fx.t.path("Library/Containers/com.docker.docker/Data/vms/0/Docker.raw").exists(),
        "failure never falls back to deletion"
    );
}

// Story "VM images: Docker prune runs only while Docker Desktop answers":
// the readiness probe before the prune commands.

#[test]
fn a_docker_desktop_that_does_not_answer_skips_its_item_and_runs_no_prune() {
    if common::unsupported("a_docker_desktop_that_does_not_answer_skips_its_item_and_runs_no_prune") {
        return;
    }
    let fx = Fx::new();
    let docker = fx.docker_desktop();
    let mut plan = fx.plan();
    select_all(&mut plan);
    let commands = FakeCommands::default().with("docker", true, "").answer("docker", DOCKER_VERSION, false, "");

    let (r, journal) = fx.run(&plan, Consent::Interactive, &commands, &FakeProcs::new(vec![]));

    let down = SkipReason::ToolNotReady {
        program: "docker".into(),
        detail: "Docker Desktop is not running or not answering".into(),
    };
    assert_eq!(outcome(&r, &docker), &Outcome::Skipped(down.clone()));
    assert_eq!(*commands.calls.borrow(), [format!("docker {DOCKER_VERSION}")], "no prune command ran");
    assert_eq!(*commands.timeouts.borrow(), [exec::READY_TIMEOUT]);
    let entry = journal.0.iter().find(|e| e.path == docker).unwrap();
    assert_eq!(entry.outcome, Outcome::Skipped(down));
    let line = serde_json::to_value(entry).unwrap();
    assert_eq!((&line["outcome"], &line["reason"]), (&"skipped".into(), &"tool-not-ready".into()));
    assert_eq!(line["detail"], "Docker Desktop is not running or not answering", "the journal line says why");
}

#[test]
fn the_probe_names_a_missing_context_or_says_what_docker_said() {
    if common::unsupported("the_probe_names_a_missing_context_or_says_what_docker_said") {
        return;
    }
    let fx = Fx::new();
    let docker = fx.docker_desktop();
    let mut plan = fx.plan();
    select_all(&mut plan);
    let said = |stderr: &str| {
        FakeCommands::default().with("docker", true, "").answer("docker", DOCKER_VERSION, false, "").stderr_on(
            "docker",
            DOCKER_VERSION,
            stderr,
        )
    };
    let skipped =
        |detail: &str| Outcome::Skipped(SkipReason::ToolNotReady { program: "docker".into(), detail: detail.into() });

    let commands = said("context \"desktop-linux\" does not exist\n");
    let (r, _) = fx.run(&plan, Consent::Interactive, &commands, &FakeProcs::new(vec![]));
    assert_eq!(outcome(&r, &docker), &skipped("the desktop-linux context does not exist"));
    assert_eq!(commands.calls.borrow().len(), 1, "{:?}", commands.calls.borrow());

    let commands =
        said("Cannot connect to the Docker daemon at unix:///x/docker.sock. Is the docker daemon running?\n");
    let (r, _) = fx.run(&plan, Consent::Interactive, &commands, &FakeProcs::new(vec![]));
    assert_eq!(outcome(&r, &docker), &skipped("Docker Desktop is not running or not answering"));

    let commands = said("Client: Docker Engine\nerror during connect: the engine is updating\n\n");
    let (r, _) = fx.run(&plan, Consent::Interactive, &commands, &FakeProcs::new(vec![]));
    assert_eq!(outcome(&r, &docker), &skipped("error during connect: the engine is updating"), "its last line");
    assert_eq!(commands.calls.borrow().len(), 1);
}

#[test]
fn a_prune_that_fails_after_a_good_probe_carries_dockers_last_line() {
    if common::unsupported("a_prune_that_fails_after_a_good_probe_carries_dockers_last_line") {
        return;
    }
    let fx = Fx::new();
    let docker = fx.docker_desktop();
    let mut plan = fx.plan();
    select_all(&mut plan);
    let image = "--context desktop-linux image prune -f";
    let commands = FakeCommands::default()
        .with("docker", true, "")
        .answer("docker", DOCKER_VERSION, true, "28.5.1\n")
        .answer("docker", image, false, "")
        .stderr_on("docker", image, "Error response from daemon: a prune operation is already running\n");

    let (r, _) = fx.run(&plan, Consent::Interactive, &commands, &FakeProcs::new(vec![]));

    let Outcome::Native { commands: ran } = outcome(&r, &docker) else { panic!("{:?}", outcome(&r, &docker)) };
    assert_eq!(ran.len(), 2, "the probe answered: both prune commands ran");
    assert!(ran[0].success && !ran[1].success);
    assert_eq!(ran[1].output, "Error response from daemon: a prune operation is already running");
    assert_eq!(commands.calls.borrow()[0], format!("docker {DOCKER_VERSION}"), "the probe first, once");
    assert_eq!(commands.calls.borrow().len(), 3);
}

#[test]
fn a_probe_that_times_out_skips_and_a_cancel_during_it_runs_no_prune() {
    if common::unsupported("a_probe_that_times_out_skips_and_a_cancel_during_it_runs_no_prune") {
        return;
    }
    let fx = Fx::new();
    let docker = fx.docker_desktop();
    let mut plan = fx.plan();
    select_all(&mut plan);

    let hung = FakeCommands::default().with("docker", true, "").error_on(
        "docker",
        DOCKER_VERSION,
        std::io::ErrorKind::TimedOut,
    );
    let (r, _) = fx.run(&plan, Consent::Interactive, &hung, &FakeProcs::new(vec![]));
    let down = SkipReason::ToolNotReady {
        program: "docker".into(),
        detail: "Docker Desktop is not running or not answering".into(),
    };
    assert_eq!(outcome(&r, &docker), &Outcome::Skipped(down));
    assert_eq!(hung.calls.borrow().len(), 1, "{:?}", hung.calls.borrow());

    // A signal while the probe runs: the probe's failure is the cancel, not a skip.
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    let probe = format!("docker {DOCKER_VERSION}");
    let commands = FakeCommands {
        on_call: Some(Box::new(move |call| {
            if call == probe {
                flag.store(true, Ordering::SeqCst);
            }
        })),
        ..FakeCommands::default().with("docker", true, "").answer("docker", DOCKER_VERSION, false, "")
    };
    let opts = ExecOptions { cancel: &cancel, ..ExecOptions::default() };
    let mut journal = MemoryJournal::default();
    let r = fx.run_with(&plan, Consent::Interactive, &commands, &FakeProcs::new(vec![]), &mut journal, &opts);
    assert_eq!(outcome(&r, &docker), &Outcome::NotRun);
    assert!(r.cancelled);
    assert_eq!(commands.calls.borrow().len(), 1, "no prune command: {:?}", commands.calls.borrow());
}

#[test]
fn read_only_trees_are_deleted() {
    if common::unsupported("read_only_trees_are_deleted") {
        return;
    }
    let fx = Fx::new();
    let modcache = fx.t.path("go/pkg/mod");
    fx.t.file("go/pkg/mod/example.com/m@v1.0.0/m.go", 2000);
    let nm = fx.node_project("p");
    for d in ["go/pkg/mod/example.com/m@v1.0.0", "go/pkg/mod/example.com", "p/node_modules/dep"] {
        common::set_mode(&fx.t.path(d), 0o555);
    }
    let plan = fx.plan();

    let (r, _) = fx.run_yes(&plan);

    assert_eq!(outcome(&r, &modcache), &Outcome::Deleted, "no go tool: falls back to delete with u+w");
    assert_eq!(outcome(&r, &nm), &Outcome::Deleted);
    assert!(!modcache.exists() && !nm.exists());
}

#[test]
fn interrupted_deletion_leaves_a_tombstone_found_as_safe() {
    if common::unsupported("interrupted_deletion_leaves_a_tombstone_found_as_safe") {
        return;
    }
    let fx = Fx::new();
    let nm = fx.node_project("p");
    let stuck = fx.t.file("p/node_modules/dep/stuck.js", 100);
    let ok = Command::new("/usr/bin/chflags").arg("uchg").arg(&stuck).status().unwrap().success();
    assert!(ok);
    let plan = fx.plan();

    let (r, _) = fx.run_yes(&plan);

    assert!(matches!(outcome(&r, &nm), Outcome::Failed { .. }));
    let tomb = fs::read_dir(fx.t.path("p"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.file_name().unwrap().to_string_lossy().contains(".ddc-deleting-"))
        .expect("tombstone left behind");
    let rescan = fx.scan();
    let f = rescan.findings.iter().find(|f| f.path == tomb).expect("tombstone is a finding");
    assert_eq!((f.rule.as_str(), f.class), ("ddc.tombstone", RiskClass::Safe));
    let _ = Command::new("/usr/bin/chflags").arg("-R").arg("nouchg").arg(&tomb).status();
}

struct Spawned(Child);

impl Drop for Spawned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn wait_until_listed(pid: u32) {
    for _ in 0..100 {
        if SystemProcesses.processes().iter().any(|p| p.pid == pid as i32 && p.exe.is_some()) {
            return;
        }
        std::thread::park_timeout(Duration::from_millis(20));
    }
}

#[test]
fn processes_using_a_finding_block_it() {
    if common::unsupported("processes_using_a_finding_block_it") {
        return;
    }
    let fx = Fx::new();
    let abs = fx.venv_project("a");
    fx.t.symlink("/bin/sleep", "a/.venv/bin/sleep");
    let rel = fx.venv_project("b");
    fx.t.symlink("/bin/sleep", "b/.venv/bin/sleep");
    let cwd = fx.node_project("c");
    let plan = fx.plan();

    let p1 = Spawned(Command::new(abs.join("bin/sleep")).arg("30").spawn().unwrap());
    let p2 = Spawned(Command::new("./.venv/bin/sleep").arg("30").current_dir(fx.t.path("b")).spawn().unwrap());
    let p3 = Spawned(Command::new("/bin/sleep").arg("30").current_dir(cwd.join("dep")).spawn().unwrap());
    for p in [&p1, &p2, &p3] {
        wait_until_listed(p.0.id());
    }

    let consent = Consent::AssumeYes { classes: vec![RiskClass::Safe, RiskClass::Rebuildable] };
    let (r, _) = fx.run(&plan, consent, &FakeCommands::default(), &SystemProcesses);

    for (path, child) in [(&abs, &p1), (&rel, &p2), (&cwd, &p3)] {
        match outcome(&r, path) {
            Outcome::Skipped(SkipReason::InUse { pid, .. }) => assert_eq!(*pid, child.0.id() as i32),
            other => panic!("{path:?}: {other:?}"),
        }
        assert!(path.exists());
    }
}

#[test]
fn keep_marker_created_after_the_scan_cancels_deletion() {
    if common::unsupported("keep_marker_created_after_the_scan_cancels_deletion") {
        return;
    }
    let fx = Fx::new();
    let top = fx.node_project("p");
    let deep = fx.node_project("q");
    let plan = fx.plan();
    fx.t.write("p/node_modules/.ddc-keep", "");
    fx.t.write("q/node_modules/dep/.ddc-keep", "");

    let (r, _) = fx.run_yes(&plan);

    assert_eq!(outcome(&r, &top), &Outcome::Skipped(SkipReason::Kept));
    assert_eq!(outcome(&r, &deep), &Outcome::Skipped(SkipReason::Kept));
}

#[test]
fn running_owner_process_skips_its_cache() {
    if common::unsupported("running_owner_process_skips_its_cache") {
        return;
    }
    let fx = Fx::new();
    let cache = fx.t.dir("Library/Caches/com.spotify.client");
    fx.t.file("Library/Caches/com.spotify.client/data", 1000);
    let plan = fx.plan();
    let spotify = app_process(4242, "/Applications/Spotify.app/Contents/MacOS/Spotify");

    let consent = Consent::AssumeYes { classes: vec![RiskClass::Safe] };
    let (r, _) = fx.run(&plan, consent, &FakeCommands::default(), &FakeProcs::new(vec![spotify]));

    assert_eq!(outcome(&r, &cache), &Outcome::Skipped(SkipReason::OwnerRunning { pid: 4242, name: "Spotify".into() }));
}

#[test]
fn an_owner_is_an_app_bundle_not_a_process_name() {
    if common::unsupported("an_owner_is_an_app_bundle_not_a_process_name") {
        return;
    }
    let fx = Fx::new();
    let cursor = fx.t.dir("Library/Application Support/Cursor/Cache");
    fx.t.file("Library/Application Support/Cursor/Cache/data", 1000);
    let firefox = fx.t.dir("Library/Caches/Firefox");
    fx.t.file("Library/Caches/Firefox/data", 1000);
    let derived = fx.t.dir("Library/Developer/Xcode/DerivedData");
    fx.t.file("Library/Developer/Xcode/DerivedData/data", 1000);
    let plan = fx.plan();
    let procs = FakeProcs::new(vec![
        // macOS's text-cursor service is not Cursor, a tool inside Xcode is not Xcode,
        // a bundle that merely starts with "Cursor" is another app, and a bare name proves nothing.
        app_process(
            1508,
            "/System/Library/PrivateFrameworks/TextInputUIMacHelper.framework/Versions/A/XPCServices/CursorUIViewService.xpc/Contents/MacOS/CursorUIViewService",
        ),
        app_process(
            51258,
            "/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/clangd",
        ),
        app_process(77, "/Applications/CursorFoo.app/Contents/MacOS/CursorFoo"),
        ProcInfo { pid: 88, name: "Firefox".into(), ..ProcInfo::default() },
    ]);

    let consent = Consent::AssumeYes { classes: vec![RiskClass::Safe] };
    let (r, _) = fx.run(&plan, consent, &FakeCommands::default(), &procs);

    assert_eq!(outcome(&r, &cursor), &Outcome::Deleted);
    assert_eq!(outcome(&r, &firefox), &Outcome::Deleted);
    assert_eq!(outcome(&r, &derived), &Outcome::Deleted);
}

#[test]
fn a_running_editor_holds_back_only_its_own_caches() {
    if common::unsupported("a_running_editor_holds_back_only_its_own_caches") {
        return;
    }
    let fx = Fx::new();
    let cursor = fx.t.dir("Library/Application Support/Cursor/GPUCache");
    fx.t.file("Library/Application Support/Cursor/GPUCache/data", 1000);
    let code = fx.t.dir("Library/Application Support/Code/GPUCache");
    fx.t.file("Library/Application Support/Code/GPUCache/data", 1000);
    let photoshop = fx.t.dir("Library/Caches/Adobe Camera Raw 2");
    fx.t.file("Library/Caches/Adobe Camera Raw 2/data", 1000);
    let plan = fx.plan();
    let procs = FakeProcs::new(vec![
        app_process(
            501,
            "/Applications/Cursor.app/Contents/Frameworks/Cursor Helper (GPU).app/Contents/MacOS/Cursor Helper (GPU)",
        ),
        app_process(
            502,
            "/Applications/Adobe Photoshop 2025/Adobe Photoshop 2025.app/Contents/MacOS/Adobe Photoshop 2025",
        ),
    ]);

    let consent = Consent::AssumeYes { classes: vec![RiskClass::Safe] };
    let (r, _) = fx.run(&plan, consent, &FakeCommands::default(), &procs);

    assert_eq!(outcome(&r, &cursor), &Outcome::Skipped(SkipReason::OwnerRunning { pid: 501, name: "Cursor".into() }));
    assert_eq!(outcome(&r, &code), &Outcome::Deleted);
    assert_eq!(
        outcome(&r, &photoshop),
        &Outcome::Skipped(SkipReason::OwnerRunning { pid: 502, name: "Adobe Photoshop 2025".into() })
    );
}

#[test]
fn no_system_process_owns_a_cache() {
    // The process table is not ported off macOS yet.
    if common::unsupported("no_system_process_owns_a_cache") {
        return;
    }
    // On the live process table: nothing Apple ships may hold back a cache, the way
    // CursorUIViewService once held back every editor's.
    let system: Vec<ProcInfo> = SystemProcesses
        .processes()
        .into_iter()
        .filter(|p| {
            p.exe.as_deref().is_some_and(|e| {
                (e.starts_with("/System") && !e.starts_with("/System/Volumes"))
                    || e.starts_with("/usr")
                    || e.starts_with("/Library/Apple")
            })
        })
        .collect();
    // With nothing to check the test proves nothing: it fails rather than pass in
    // silence (ADR "Platform seam", §9, amended: a skip names its test, or it is
    // a failure).
    assert!(!system.is_empty(), "no system process with a readable executable path");
    let mut bundled = 0;
    for p in &system {
        let Some(app) = p.app_bundle() else { continue };
        bundled += 1;
        for rule in RULES {
            assert!(
                !rule.owners.iter().any(|o| owns(o, &app) || owns(o, &p.name)),
                "{} ({:?}) would hold back {}",
                app,
                p.exe,
                rule.id
            );
        }
    }
    assert!(bundled > 0, "no system process runs from an app bundle");
}

#[test]
fn a_renamed_bundle_is_recognised_by_its_executable() {
    if common::unsupported("a_renamed_bundle_is_recognised_by_its_executable") {
        return;
    }
    let fx = Fx::new();
    let chrome = fx.t.dir("Library/Caches/Google/Chrome");
    fx.t.file("Library/Caches/Google/Chrome/data", 1000);
    let derived = fx.t.dir("Library/Developer/Xcode/DerivedData");
    fx.t.file("Library/Developer/Xcode/DerivedData/data", 1000);
    let plan = fx.plan();
    let procs = FakeProcs::new(vec![
        app_process(
            601,
            "/Applications/Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/141.0/Helpers/Google Chrome Helper (Renderer).app/Contents/MacOS/Google Chrome Helper (Renderer)",
        ),
        app_process(602, "/Applications/Xcode16.app/Contents/MacOS/Xcode"),
    ]);

    let consent = Consent::AssumeYes { classes: vec![RiskClass::Safe] };
    let (r, _) = fx.run(&plan, consent, &FakeCommands::default(), &procs);

    assert_eq!(outcome(&r, &chrome), &Outcome::Skipped(SkipReason::OwnerRunning { pid: 601, name: "Chrome".into() }));
    assert_eq!(outcome(&r, &derived), &Outcome::Skipped(SkipReason::OwnerRunning { pid: 602, name: "Xcode16".into() }));
}

#[test]
fn native_command_runs_only_where_the_tool_reports_the_finding() {
    if common::unsupported("native_command_runs_only_where_the_tool_reports_the_finding") {
        return;
    }
    let fx = Fx::new();
    let uv = fx.t.dir(".cache/uv");
    fx.t.file(".cache/uv/wheels/a.whl", 1000);
    let plan = fx.plan();
    let consent = Consent::AssumeYes { classes: vec![RiskClass::Safe] };

    let here = FakeCommands::default().with("uv", true, &format!("{}\n", uv.display()));
    let (r, _) = fx.run(&plan, consent.clone(), &here, &FakeProcs::new(vec![]));
    assert!(matches!(outcome(&r, &uv), Outcome::Native { commands } if commands[0].success));
    assert_eq!(*here.calls.borrow(), vec!["uv cache dir".to_owned(), "uv cache clean".to_owned()]);

    let elsewhere = FakeCommands::default().with("uv", true, "/somewhere/else\n");
    let (r, _) = fx.run(&plan, consent, &elsewhere, &FakeProcs::new(vec![]));
    assert_eq!(outcome(&r, &uv), &Outcome::Deleted, "another location: falls back to delete");
    assert_eq!(*elsewhere.calls.borrow(), vec!["uv cache dir".to_owned()], "uv cache clean is not run");
}

#[test]
fn review_items_default_to_the_trash_unless_permanent() {
    if common::unsupported("review_items_default_to_the_trash_unless_permanent") {
        return;
    }
    let fx = Fx::new();
    let venv = fx.t.path("nolock/.venv");
    fx.t.write("nolock/.venv/pyvenv.cfg", "x");
    let report = fx.scan();
    let catalog = Catalog::builtin(fx.t.root());

    let action = |permanent| {
        let plan = build_plan(&report, &catalog, &PlanOptions { permanent, ..PlanOptions::default() });
        plan.items.iter().find(|i| i.finding.path == venv).map(|i| i.action).unwrap()
    };

    assert_eq!(report.findings.iter().find(|f| f.path == venv).unwrap().class, RiskClass::Review);
    assert_eq!(action(false), ddc_core::plan::Action::Trash);
    assert_eq!(action(true), ddc_core::plan::Action::Delete { make_writable: false });

    let mut plan = build_plan(&report, &catalog, &PlanOptions { permanent: true, ..PlanOptions::default() });
    select_all(&mut plan);
    let (r, _) = fx.run(&plan, Consent::Interactive, &FakeCommands::default(), &FakeProcs::new(vec![]));
    assert_eq!(outcome(&r, &venv), &Outcome::Deleted);
    assert!(!venv.exists() && !fx.data.path().join("FakeTrash").exists(), "deleted, not trashed");
}

#[test]
fn user_data_and_report_only_rules_never_enter_a_plan() {
    if common::unsupported("user_data_and_report_only_rules_never_enter_a_plan") {
        return;
    }
    let fx = Fx::new();
    fx.t.file("Pictures/a.jpg", 100);
    fx.t.file(".Trash/old.zip", 100);
    fx.t.file(".colima/default/disk.img", 100);
    fx.t.file("Library/Developer/Xcode/Archives/a.xcarchive/x", 100);

    let plan = fx.plan();

    assert!(plan.items.is_empty(), "{:?}", plan.items.iter().map(|i| &i.finding.rule).collect::<Vec<_>>());
}

fn forged(
    plan: &Plan,
    report: &ScanReport,
    rel_path: &Path,
    action: ddc_core::plan::Action,
) -> ddc_core::plan::PlanItem {
    let finding = report.findings.iter().find(|f| f.path == rel_path).expect("finding").clone();
    let _ = plan;
    ddc_core::plan::PlanItem { finding, action, default_selected: true, selected: true }
}

#[test]
fn the_executor_refuses_actions_the_rule_does_not_allow() {
    if common::unsupported("the_executor_refuses_actions_the_rule_does_not_allow") {
        return;
    }
    let fx = Fx::new();
    fx.t.file("Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw", 50_000);
    fx.t.file("Library/Application Support/MobileSync/Backup/device/Manifest.db", 5_000);
    let docker = fx.t.path("Library/Containers/com.docker.docker");
    let backups = fx.t.path("Library/Application Support/MobileSync/Backup");
    let report = fx.scan();
    let mut plan = build_plan(&report, &Catalog::builtin(fx.t.root()), &PlanOptions::default());
    use ddc_core::plan::Action;
    plan.items = vec![
        forged(&plan, &report, &docker, Action::Delete { make_writable: false }),
        forged(&plan, &report, &backups, Action::Delete { make_writable: false }),
    ];
    let (r, _) = fx.run(&plan, Consent::Interactive, &FakeCommands::default(), &FakeProcs::new(vec![]));
    assert_eq!(outcome(&r, &docker), &Outcome::Skipped(SkipReason::ActionNotAllowed));
    assert_eq!(outcome(&r, &backups), &Outcome::Skipped(SkipReason::ActionNotAllowed));

    plan.items = vec![forged(&plan, &report, &docker, Action::Trash)];
    let (r, _) = fx.run(&plan, Consent::Interactive, &FakeCommands::default(), &FakeProcs::new(vec![]));
    assert_eq!(outcome(&r, &docker), &Outcome::Skipped(SkipReason::ActionNotAllowed));
    assert!(fx.t.path("Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw").exists());
    assert!(backups.join("device/Manifest.db").exists());
}

#[test]
fn a_known_path_that_became_a_git_work_tree_is_refused() {
    if common::unsupported("a_known_path_that_became_a_git_work_tree_is_refused") {
        return;
    }
    let fx = Fx::new();
    let uv = fx.t.dir(".cache/uv");
    fx.t.file(".cache/uv/x", 100);
    let plan = fx.plan();
    fx.t.dir(".cache/uv/.git");

    let (r, _) = fx.run_yes(&plan);

    assert_eq!(outcome(&r, &uv), &Outcome::Skipped(SkipReason::ProjectRoot));
}

#[test]
fn a_parent_replaced_by_a_symlink_is_refused() {
    if common::unsupported("a_parent_replaced_by_a_symlink_is_refused") {
        return;
    }
    let fx = Fx::new();
    let nm = fx.node_project("p");
    let plan = fx.plan();
    fs::rename(fx.t.path("p"), fx.t.path("p-real")).unwrap();
    common::symlink(fx.t.path("p-real"), fx.t.path("p"));

    let (r, _) = fx.run_yes(&plan);

    assert_eq!(outcome(&r, &nm), &Outcome::Skipped(SkipReason::ParentChanged));
    assert!(fx.t.path("p-real/node_modules").exists());
}

#[test]
fn a_relative_executable_with_unknown_cwd_blocks_same_named_findings() {
    if common::unsupported("a_relative_executable_with_unknown_cwd_blocks_same_named_findings") {
        return;
    }
    let fx = Fx::new();
    let venv = fx.venv_project("a");
    let plan = fx.plan();
    let python = ProcInfo {
        pid: 77,
        name: "python".into(),
        exe: Some(PathBuf::from("./.venv/bin/python")),
        ..ProcInfo::default()
    };

    let consent = Consent::AssumeYes { classes: vec![RiskClass::Safe, RiskClass::Rebuildable] };
    let (r, _) = fx.run(&plan, consent, &FakeCommands::default(), &FakeProcs::new(vec![python]));

    assert_eq!(outcome(&r, &venv), &Outcome::Skipped(SkipReason::InUseUnknownCwd { pid: 77, name: "python".into() }));
}

#[test]
fn assume_yes_skips_active_projects_even_if_selected() {
    if common::unsupported("assume_yes_skips_active_projects_even_if_selected") {
        return;
    }
    let fx = Fx::new();
    let t = &fx.t;
    t.write("fresh/package.json", "{}");
    t.write("fresh/package-lock.json", "{}");
    t.file("fresh/node_modules/x.js", 100);
    let nm = t.path("fresh/node_modules");
    let mut plan = fx.plan();
    assert!(!plan.items.iter().find(|i| i.finding.path == nm).unwrap().default_selected, "edited today: active");
    select_all(&mut plan);

    let (r, _) = fx.run_yes(&plan);

    assert_eq!(outcome(&r, &nm), &Outcome::Skipped(SkipReason::NeedsConfirmation));
    assert!(nm.exists());
}

#[test]
fn a_directory_swapped_while_a_tool_runs_is_not_deleted() {
    if common::unsupported("a_directory_swapped_while_a_tool_runs_is_not_deleted") {
        return;
    }
    let fx = Fx::new();
    let uv = fx.t.dir(".cache/uv");
    fx.t.file(".cache/uv/wheels/a.whl", 1000);
    fx.t.file("precious/thesis.tex", 100);
    let plan = fx.plan();
    let (cache, precious) = (uv.clone(), fx.t.path("precious"));
    let swap = FakeCommands {
        on_run: Some(Box::new(move || {
            if precious.exists() {
                fs::rename(&cache, cache.with_file_name("uv-moved")).unwrap();
                fs::rename(&precious, &cache).unwrap();
            }
        })),
        ..FakeCommands::default().with("uv", true, "/somewhere/else\n")
    };

    let (r, _) = fx.run(&plan, Consent::AssumeYes { classes: vec![RiskClass::Safe] }, &swap, &FakeProcs::new(vec![]));

    assert_eq!(outcome(&r, &uv), &Outcome::Skipped(SkipReason::ChangedSinceScan));
    assert!(uv.join("thesis.tex").exists(), "the swapped-in directory survives");
}

#[test]
fn partly_unreadable_findings_are_not_deleted() {
    if common::unsupported("partly_unreadable_findings_are_not_deleted") {
        return;
    }
    let fx = Fx::new();
    let nm = fx.node_project("p");
    fx.t.file("p/node_modules/secret/x.js", 100);
    let locked = fx.t.lock("p/node_modules/secret");
    if fs::read_dir(&locked).is_ok() {
        eprintln!("SKIPPED: running as root");
        return;
    }
    let plan = fx.plan();

    let (r, _) = fx.run_yes(&plan);

    assert_eq!(outcome(&r, &nm), &Outcome::Skipped(SkipReason::Incomplete));
}

// Story "Progress output while ddc clean runs": the counter the terminal's
// progress line and the app's progress events read.

#[test]
fn progress_reaches_the_totals_with_native_only_bytes_left_out() {
    if common::unsupported("progress_reaches_the_totals_with_native_only_bytes_left_out") {
        return;
    }
    let fx = Fx::new();
    let deleted = fx.node_project("p");
    let kept = fx.node_project("q");
    fx.venv_project("r");
    fx.t.file("Library/Containers/com.docker.docker/Data/vms/0/Docker.raw", 50_000);
    let mut plan = fx.plan();
    select_all(&mut plan);
    fx.t.write("q/node_modules/.ddc-keep", "");
    let progress = ExecProgress::default();
    let opts = ExecOptions { progress: Some(&progress), ..ExecOptions::default() };
    let docker = FakeCommands::default().with("docker", true, "").answer("docker", DOCKER_VERSION, true, "28.5.1\n");

    let r = fx.run_with(
        &plan,
        Consent::Interactive,
        &docker,
        &FakeProcs::new(vec![]),
        &mut MemoryJournal::default(),
        &opts,
    );

    let chosen: Vec<_> = plan.items.iter().filter(|i| i.selected).collect();
    let native_only = |a: &Action| matches!(a, Action::NativeOnly(_));
    assert!(chosen.iter().any(|i| native_only(&i.action)), "the fixture holds a native-only item");
    let counted: u64 = chosen.iter().filter(|i| !native_only(&i.action)).map(|i| i.finding.bytes).sum();
    assert!(counted < chosen.iter().map(|i| i.finding.bytes).sum::<u64>());
    let s = progress.snapshot();
    assert_eq!(s.phase, ExecPhase::Cleaning);
    assert_eq!((s.items_done, s.items_total), (chosen.len() as u64, chosen.len() as u64));
    assert_eq!((s.bytes_done, s.bytes_total), (counted, counted), "native-only bytes are in neither");
    assert_eq!(s.current, None);
    assert_eq!(outcome(&r, &deleted), &Outcome::Deleted);
    assert_eq!(outcome(&r, &kept), &Outcome::Skipped(SkipReason::Kept), "a skipped item counts as processed");
}

/// Sets the cancel flag when the first item is journaled.
struct CancelOnFirst<'a> {
    flag: &'a AtomicBool,
    inner: MemoryJournal,
}

impl Journal for CancelOnFirst<'_> {
    fn record(&mut self, entry: &JournalEntry) -> std::io::Result<()> {
        self.flag.store(true, Ordering::SeqCst);
        self.inner.record(entry)
    }
}

#[test]
fn a_cancel_after_the_first_item_counts_only_that_one() {
    if common::unsupported("a_cancel_after_the_first_item_counts_only_that_one") {
        return;
    }
    let fx = Fx::new();
    for p in ["p", "q", "r"] {
        fx.node_project(p);
    }
    let plan = fx.plan();
    let flag = AtomicBool::new(false);
    let mut journal = CancelOnFirst { flag: &flag, inner: MemoryJournal::default() };
    let progress = ExecProgress::default();
    let opts = ExecOptions { cancel: &flag, progress: Some(&progress), ..ExecOptions::default() };

    let r = fx.run_with(&plan, yes(), &FakeCommands::default(), &FakeProcs::new(vec![]), &mut journal, &opts);

    assert!(r.cancelled);
    let s = progress.snapshot();
    assert_eq!((s.items_done, s.items_total), (1, 3), "not-run items are not processed");
    assert_eq!(s.current, None);
}

/// Sets the cancel flag on the `k`-th item's event, as a SIGTERM read with
/// that event does in the CLI (`interrupt_cli.rs`).
#[derive(Debug)]
struct CancelOnItem<'a> {
    k: u64,
    flag: &'a AtomicBool,
}

impl ExecEvents for CancelOnItem<'_> {
    fn item(&self, i: u64, _: u64, _: &PlanItem, _: &Outcome) {
        if i == self.k {
            self.flag.store(true, Ordering::SeqCst);
        }
    }
}

#[test]
fn a_cancel_at_the_kth_item_event_leaves_every_later_item_not_run() {
    if common::unsupported("a_cancel_at_the_kth_item_event_leaves_every_later_item_not_run") {
        return;
    }
    let fx = Fx::new();
    for i in 0..6 {
        fx.node_project(&format!("p{i}"));
    }
    let plan = fx.plan();
    let flag = AtomicBool::new(false);
    let events = CancelOnItem { k: 2, flag: &flag };
    let opts = ExecOptions { cancel: &flag, events: Some(&events), ..ExecOptions::default() };
    let mut journal = MemoryJournal::default();

    let r = fx.run_with(&plan, yes(), &FakeCommands::default(), &FakeProcs::new(vec![]), &mut journal, &opts);

    assert!(r.cancelled);
    assert_eq!(r.items.len(), 6, "{:#?}", r.items);
    for (n, item) in r.items.iter().enumerate() {
        if n < 2 {
            assert_eq!(item.outcome, Outcome::Deleted, "{:?}", item.path);
            assert!(!item.path.exists());
        } else {
            assert_eq!(item.outcome, Outcome::NotRun, "{:?}", item.path);
            assert!(item.path.exists());
        }
    }
    let journaled: Vec<&Path> = journal.0.iter().map(|e| e.path.as_path()).collect();
    let deleted: Vec<&Path> = r.items[..2].iter().map(|i| i.path.as_path()).collect();
    assert_eq!(journaled, deleted, "a not-run item is never journaled");
}

/// Reads the shared counter when the guard takes its process snapshot:
/// after the keep-marker walk, before the first item.
struct SnapshotOnFirstCall<'a> {
    progress: &'a ExecProgress,
    seen: RefCell<Option<ExecSnapshot>>,
}

impl ProcessTable for SnapshotOnFirstCall<'_> {
    fn processes(&self) -> Vec<ProcInfo> {
        self.seen.borrow_mut().get_or_insert_with(|| self.progress.snapshot());
        Vec::new()
    }
    fn open_files(&self, _: i32) -> Vec<PathBuf> {
        Vec::new()
    }
    fn process_start(&self, _: i32) -> Option<ProcStart> {
        None
    }
    fn open_vnodes(&self, _: i32) -> Vec<OpenVnode> {
        Vec::new()
    }
}

#[test]
fn preparing_counts_the_files_of_the_chosen_trees_before_the_first_item() {
    if common::unsupported("preparing_counts_the_files_of_the_chosen_trees_before_the_first_item") {
        return;
    }
    let fx = Fx::new();
    fx.node_project("p");
    fx.venv_project("q");
    let plan = fx.plan();
    let files: u64 = plan.items.iter().filter(|i| i.selected).map(|i| i.finding.files).sum();
    let progress = ExecProgress::default();
    let procs = SnapshotOnFirstCall { progress: &progress, seen: RefCell::new(None) };
    let opts = ExecOptions { progress: Some(&progress), ..ExecOptions::default() };

    fx.run_with(&plan, yes(), &FakeCommands::default(), &procs, &mut MemoryJournal::default(), &opts);

    let seen = procs.seen.into_inner().expect("the guard read the process table");
    assert_eq!(seen.phase, ExecPhase::Preparing);
    assert!(files >= 3, "{files}");
    assert_eq!((seen.walked.files, seen.walk_expected), (files, files));
    assert_eq!((seen.items_done, seen.current), (0, None));
}

#[test]
fn the_running_tool_is_named_while_its_command_runs() {
    if common::unsupported("the_running_tool_is_named_while_its_command_runs") {
        return;
    }
    let fx = Fx::new();
    let uv = fx.t.dir(".cache/uv");
    fx.t.file(".cache/uv/wheels/a.whl", 1000);
    let mut plan = fx.plan();
    select_all(&mut plan);
    let progress = Arc::new(ExecProgress::default());
    let tools = Arc::new(Mutex::new(Vec::new()));
    let (p, seen) = (Arc::clone(&progress), Arc::clone(&tools));
    let commands = FakeCommands {
        on_run: Some(Box::new(move || seen.lock().unwrap().push(p.snapshot().current.and_then(|c| c.tool)))),
        ..FakeCommands::default().with("uv", true, &format!("{}\n", uv.display()))
    };
    let opts = ExecOptions { progress: Some(&progress), ..ExecOptions::default() };
    let mut journal = ToolAtRecord { progress: &progress, seen: Vec::new(), inner: MemoryJournal::default() };

    let r = fx.run_with(&plan, Consent::Interactive, &commands, &FakeProcs::new(vec![]), &mut journal, &opts);

    assert!(matches!(outcome(&r, &uv), Outcome::Native { .. }), "{:?}", outcome(&r, &uv));
    assert_eq!(*commands.calls.borrow(), ["uv cache dir", "uv cache clean"]);
    assert_eq!(*tools.lock().unwrap(), [Some("uv"), Some("uv")], "named for the probe and the command");
    assert_eq!(journal.seen, [Some(None)], "the item is still current when journaled, its tool no longer named");
    assert_eq!(progress.snapshot().current, None, "cleared after the item");
}

/// Reads the current item's tool when the item is journaled: after the
/// action, before the counter finishes the item.
struct ToolAtRecord<'a> {
    progress: &'a ExecProgress,
    seen: Vec<Option<Option<&'static str>>>,
    inner: MemoryJournal,
}

impl Journal for ToolAtRecord<'_> {
    fn record(&mut self, entry: &JournalEntry) -> std::io::Result<()> {
        self.seen.push(self.progress.snapshot().current.map(|c| c.tool));
        self.inner.record(entry)
    }
}

#[test]
fn local_snapshots_counted_by_the_caller_run_no_tmutil() {
    if common::unsupported("local_snapshots_counted_by_the_caller_run_no_tmutil") {
        return;
    }
    let fx = Fx::new();
    fx.node_project("p");
    let plan = fx.plan();
    let tmutil = || FakeCommands::default().with("tmutil", true, "com.apple.TimeMachine.2026-10-25-000000.local\n");

    let counted = tmutil();
    let r = fx.run_with(
        &plan,
        yes(),
        &counted,
        &FakeProcs::new(vec![]),
        &mut MemoryJournal::default(),
        &ExecOptions::default(),
    );
    assert_eq!(r.local_snapshots, 1);
    assert_eq!(*counted.calls.borrow(), ["tmutil listlocalsnapshots /"]);

    let given = tmutil();
    let opts = ExecOptions { local_snapshots: Some(3), ..ExecOptions::default() };
    let r = fx.run_with(&plan, yes(), &given, &FakeProcs::new(vec![]), &mut MemoryJournal::default(), &opts);
    assert_eq!(r.local_snapshots, 3);
    assert!(given.calls.borrow().is_empty(), "{:?}", given.calls.borrow());
}

// Story "CLI: progress events": the hook that writes the lines that must
// reach stderr in order.

/// The hook's calls and the journal's records, in one log.
#[derive(Debug, Default)]
struct Recorder {
    log: Arc<Mutex<Vec<String>>>,
}

impl ExecEvents for Recorder {
    fn cleaning(&self) {
        self.log.lock().unwrap().push("cleaning".to_owned());
    }
    fn item(&self, i: u64, n: u64, item: &PlanItem, outcome: &Outcome) {
        let bytes = ddc_core::progress::ItemEvent::new(i, n, item, outcome).bytes;
        self.log.lock().unwrap().push(format!("item {i}/{n} {bytes}"));
    }
}

struct LoggedJournal {
    log: Arc<Mutex<Vec<String>>>,
    inner: MemoryJournal,
}

impl Journal for LoggedJournal {
    fn record(&mut self, entry: &JournalEntry) -> std::io::Result<()> {
        self.log.lock().unwrap().push("journal".to_owned());
        self.inner.record(entry)
    }
}

#[test]
fn the_hook_says_cleaning_before_the_first_action_and_each_item_after_its_record() {
    if common::unsupported("the_hook_says_cleaning_before_the_first_action_and_each_item_after_its_record") {
        return;
    }
    let fx = Fx::new();
    fx.node_project("p");
    fx.node_project("q");
    fx.t.file("Library/Containers/com.docker.docker/Data/vms/0/Docker.raw", 50_000);
    let mut plan = fx.plan();
    select_all(&mut plan);
    let recorder = Recorder::default();
    let mut journal = LoggedJournal { log: Arc::clone(&recorder.log), inner: MemoryJournal::default() };
    let progress = ExecProgress::default();
    let opts = ExecOptions { progress: Some(&progress), events: Some(&recorder), ..ExecOptions::default() };
    let docker = FakeCommands::default().with("docker", true, "").answer("docker", DOCKER_VERSION, true, "28.5.1\n");

    fx.run_with(&plan, Consent::Interactive, &docker, &FakeProcs::new(vec![]), &mut journal, &opts);

    let chosen: Vec<_> = plan.items.iter().filter(|i| i.selected).collect();
    let n = chosen.len();
    let log = recorder.log.lock().unwrap().clone();
    let mut expected = vec!["cleaning".to_owned()];
    for (i, item) in (1..).zip(&chosen) {
        expected.push("journal".to_owned());
        expected.push(format!("item {i}/{n} {}", item.finding.bytes));
    }
    assert_eq!(log, expected, "cleaning once, before the first record; each item after its own");
    let native_only: Vec<_> = chosen.iter().filter(|i| matches!(i.action, Action::NativeOnly(_))).collect();
    assert_eq!(native_only.len(), 1);
    assert!(native_only[0].finding.bytes > 0, "a native-only item carries its bytes on its item event");
    let counted: u64 =
        chosen.iter().filter(|i| !matches!(i.action, Action::NativeOnly(_))).map(|i| i.finding.bytes).sum();
    assert_eq!(progress.snapshot().bytes_total, counted, "but not in the byte total");
}

#[test]
fn the_hook_says_cleaning_with_no_items_and_nothing_when_execute_refuses() {
    if common::unsupported("the_hook_says_cleaning_with_no_items_and_nothing_when_execute_refuses") {
        return;
    }
    let fx = Fx::new();
    fx.node_project("p");
    let mut plan = fx.plan();
    plan.items.iter_mut().for_each(|i| i.selected = false);
    let recorder = Recorder::default();
    let opts = ExecOptions { events: Some(&recorder), ..ExecOptions::default() };
    fx.run_with(&plan, yes(), &FakeCommands::default(), &FakeProcs::new(vec![]), &mut MemoryJournal::default(), &opts);
    assert_eq!(*recorder.log.lock().unwrap(), ["cleaning"]);

    let refused = Recorder::default();
    let opts = ExecOptions { events: Some(&refused), ..ExecOptions::default() };
    let trash = FakeTrash(fx.data.path().join("FakeTrash"));
    let mut journal = MemoryJournal::default();
    let result = exec::execute(
        &fx.env(),
        &Catalog::builtin(fx.t.root()),
        &fx.plan(),
        &Consent::AssumeYes { classes: vec![RiskClass::Review] },
        Ports {
            commands: &FakeCommands::default(),
            trash: &trash,
            procs: &FakeProcs::new(vec![]),
            journal: &mut journal,
        },
        &opts,
    );
    assert!(matches!(result, Err(ExecError::ReviewWithYes)));
    assert!(refused.log.lock().unwrap().is_empty());
}

#[test]
fn a_cancelled_marker_walk_keeps_its_target() {
    if common::unsupported("a_cancelled_marker_walk_keeps_its_target") {
        return;
    }
    // The ticker cancels the walk on Ctrl-C; whichever flag the executor sees
    // first, a target walked only in part must not be cleaned.
    let fx = Fx::new();
    let nm = fx.node_project("p");
    let plan = fx.plan();
    let progress = ExecProgress::default();
    progress.cancel_walk();
    let opts = ExecOptions { progress: Some(&progress), ..ExecOptions::default() };

    let r = fx.run_with(
        &plan,
        yes(),
        &FakeCommands::default(),
        &FakeProcs::new(vec![]),
        &mut MemoryJournal::default(),
        &opts,
    );

    assert_eq!(outcome(&r, &nm), &Outcome::Skipped(SkipReason::Kept));
    assert!(nm.exists());
}
