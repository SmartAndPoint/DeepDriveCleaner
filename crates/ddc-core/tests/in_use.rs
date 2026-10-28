//! Story "Release 0.2.1: the Review window in the Dock, and skip what a
//! running process holds open": open files in the in-use check (safety ADR,
//! §5 and its 0.2.1 amendment). Temporary homes; the commands, the Trash and
//! the process table are fakes, and every case runs as `clean --items` and as
//! its dry run, which must agree.

mod common;

use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use common::TreeBuilder;
use common::fakes::{ChangingProcs, FakeCommands, FakeProcs, FakeTrash, app_process, holder};
use ddc_core::class::RiskClass;
use ddc_core::env::Env;
use ddc_core::exec::preflight::{self, Prediction, PreflightLine, PreflightPorts};
use ddc_core::exec::report::CleanReport;
use ddc_core::exec::{self, Consent, ExecOptions, MemoryJournal, Outcome, Ports, ProcessTable, SkipReason};
use ddc_core::model::ScanReport;
use ddc_core::plan::{ActionKind, ListedItem, plan_listed};
use ddc_core::report::volumes::SystemStatFs;
use ddc_core::report::{self, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::scan::{Progress, StdReader, WalkOptions};
use ddc_core::sys::procs::ProcInfo;

const HOUR: Duration = Duration::from_secs(3_600);

/// Docker Desktop's readiness probe, as the fake runner keys it.
const DOCKER_VERSION: &str = "--context desktop-linux version --format {{.Server.Version}}";
/// The desktop-linux context's metadata, under `~/.docker`.
const DOCKER_CONTEXT: &str =
    ".docker/contexts/meta/fe9c6bd7a66301f49ca9b6a70b217107cd1284598bfc254700c989b916da791e/meta.json";

struct Home {
    tree: TreeBuilder,
    data: tempfile::TempDir,
    /// An hour after the fixtures were written, so no entry is "recent".
    now: SystemTime,
}

impl Home {
    fn new() -> Self {
        Self { tree: TreeBuilder::new(), data: tempfile::tempdir().unwrap(), now: SystemTime::now() + HOUR }
    }

    fn env(&self) -> Env {
        Env::with_paths(self.tree.root(), self.data.path(), &self.data.path().join("logs"), self.now).unwrap()
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.tree.path(rel)
    }

    fn catalog(&self) -> Catalog {
        Catalog::builtin(self.tree.root())
    }

    fn scan(&self) -> ScanReport {
        let opts =
            ScanOptions { walk: WalkOptions { threads: Some(4), ..WalkOptions::default() }, ..Default::default() };
        report::scan_with_entries(&self.env(), &opts, &Progress::default(), &SystemStatFs).unwrap().0
    }

    /// An idle Node project; its `node_modules` is deleted.
    fn node_modules(&self) -> PathBuf {
        self.tree.write("p/package.json", "{}");
        self.tree.write("p/package-lock.json", "{}");
        self.tree.file("p/node_modules/dep/index.js", 20_000);
        self.path("p/node_modules")
    }

    /// uv's cache, cleaned with `uv cache clean`.
    fn uv_cache(&self) -> PathBuf {
        self.tree.file(".cache/uv/wheels/a.whl", 1_000);
        self.tree.write(".cache/uv/.lock", "");
        self.path(".cache/uv")
    }

    /// A finding as the app lists it, with the action the clean will take.
    fn listed(&self, path: &Path, action: ActionKind, permanent: bool) -> ListedItem {
        let scanned = self.scan();
        let f = scanned.findings.iter().find(|f| f.path == path).unwrap_or_else(|| panic!("no finding at {path:?}"));
        ListedItem {
            path: f.path.clone(),
            dev: f.dev,
            ino: f.ino,
            rule: f.rule.clone(),
            class: f.class,
            action,
            permanent,
        }
    }

    /// An entry of a user-data folder, picked by hand.
    fn pick(&self, rel: &str, rule: &str) -> ListedItem {
        let path = self.path(rel);
        let md = fs::symlink_metadata(&path).unwrap();
        ListedItem {
            path,
            dev: md.dev(),
            ino: md.ino(),
            rule: rule.to_owned(),
            class: RiskClass::UserData,
            action: ActionKind::Trash,
            permanent: false,
        }
    }

    fn dry_run(&self, items: &[ListedItem], commands: &FakeCommands, procs: &dyn ProcessTable) -> Vec<Prediction> {
        let mut out = Vec::new();
        let ports = PreflightPorts { commands, procs, reader: &StdReader };
        preflight::preflight(&self.env(), &self.catalog(), &[], &[], items, &ports, &mut |line| {
            if let PreflightLine::Item(i) = line {
                out.push(i.prediction.clone());
            }
            Ok(())
        })
        .unwrap();
        out
    }

    /// Rescans, plans the list and runs it as `clean --items` does.
    fn clean(&self, items: &[ListedItem], commands: &FakeCommands, procs: &dyn ProcessTable) -> Vec<Outcome> {
        let (scanned, entries) = report::scan_with_entries(
            &self.env(),
            &ScanOptions { walk: WalkOptions { threads: Some(4), ..WalkOptions::default() }, ..Default::default() },
            &Progress::default(),
            &SystemStatFs,
        )
        .unwrap();
        let catalog = self.catalog();
        let listed = plan_listed(&scanned, &entries, &catalog, items);
        let mut journal = MemoryJournal::default();
        let run = exec::execute(
            &self.env(),
            &catalog,
            &listed.plan,
            &Consent::Listed,
            Ports { commands, trash: &FakeTrash(self.data.path().join("FakeTrash")), procs, journal: &mut journal },
            &ExecOptions { cancel: &AtomicBool::new(false), ..ExecOptions::default() },
        )
        .unwrap();
        let report = CleanReport::new(items, &listed.refused, &run, PathBuf::from("/journal"));
        report.items.into_iter().map(|i| i.outcome).collect()
    }

    /// The dry run with `predict`, then the clean with `clean`: each
    /// prediction must be its outcome.
    fn predict_and_clean(
        &self,
        items: &[ListedItem],
        commands: &FakeCommands,
        predict: &dyn ProcessTable,
        clean: &dyn ProcessTable,
    ) -> Vec<Outcome> {
        let predicted = self.dry_run(items, commands, predict);
        let outcomes = self.clean(items, commands, clean);
        for ((item, p), o) in items.iter().zip(&predicted).zip(&outcomes) {
            match (p, o) {
                (Prediction::WouldTrash, Outcome::Trashed)
                | (Prediction::WouldDelete, Outcome::Deleted)
                | (Prediction::WouldRun, Outcome::Native { .. }) => {}
                (Prediction::WouldSkip(a), Outcome::Skipped(b)) if a == b => {}
                _ => panic!("{}: the dry run said {p:?}, the clean did {o:?}", item.path.display()),
            }
        }
        outcomes
    }
}

/// Docker Desktop's backend, working inside its own folder.
fn docker_backend(docker: &Path) -> ProcInfo {
    ProcInfo {
        cwd: Some(docker.join("Data")),
        open_files: vec![docker.join("Data/vms/0/Docker.raw")],
        ..app_process(707, "/Applications/Docker.app/Contents/MacOS/com.docker.backend")
    }
}

fn in_use(pid: i32, name: &str) -> Outcome {
    Outcome::Skipped(SkipReason::InUse { pid, name: name.into() })
}

#[test]
fn a_file_held_open_inside_a_deleted_finding_skips_it_and_the_dry_run_says_so() {
    let home = Home::new();
    let nm = home.node_modules();
    let item = home.listed(&nm, ActionKind::Delete, false);
    let node = FakeProcs(vec![holder(701, "node", &[&nm.join("dep/index.js")])]);

    let outcomes = home.predict_and_clean(std::slice::from_ref(&item), &FakeCommands::default(), &node, &node);
    assert_eq!(outcomes, [in_use(701, "node")]);
    assert!(nm.exists());

    let none = FakeProcs(vec![]);
    assert_eq!(home.predict_and_clean(&[item], &FakeCommands::default(), &none, &none), [Outcome::Deleted]);
    assert!(!nm.exists());
}

#[test]
fn a_file_closed_by_the_confirming_read_does_not_skip() {
    let home = Home::new();
    let nm = home.node_modules();
    let item = home.listed(&nm, ActionKind::Delete, false);
    // A passing reader: open in the snapshot, closed 250 ms later.
    let passing = ChangingProcs::new(vec![holder(702, "mdworker_shared", &[&nm.join("dep/index.js")])], vec![]);

    assert_eq!(home.clean(&[item], &FakeCommands::default(), &passing), [Outcome::Deleted]);
    assert!(passing.reads.get() >= 2, "the hit was read again");
}

#[test]
fn a_tool_command_never_starts_while_a_process_holds_its_cache_open() {
    let home = Home::new();
    let uv = home.uv_cache();
    let item = home.listed(&uv, ActionKind::Native, false);
    let lock = uv.join(".lock");
    let tool = || FakeCommands::default().with("uv", true, &format!("{}\n", uv.display()));

    // Held from the start: the guard skips it, and the dry run predicts it.
    let commands = tool();
    let running = FakeProcs(vec![holder(703, "uv", &[&lock])]);
    assert_eq!(home.predict_and_clean(std::slice::from_ref(&item), &commands, &running, &running), [in_use(703, "uv")]);
    assert!(commands.calls.borrow().is_empty(), "neither the probe nor the clean ran: {:?}", commands.calls.borrow());

    // Taken after the snapshot: the read right before the command sees it.
    let commands = tool();
    let later = ChangingProcs::new(vec![], vec![holder(704, "uv", &[&lock])]);
    assert_eq!(home.clean(std::slice::from_ref(&item), &commands, &later), [in_use(704, "uv")]);
    assert!(commands.calls.borrow().is_empty(), "{:?}", commands.calls.borrow());

    // Nobody holds it: the tool runs, with the lock timeout for uv only.
    let commands = tool();
    let outcomes = home.clean(&[item], &commands, &FakeProcs(vec![]));
    assert!(matches!(&outcomes[..], [Outcome::Native { commands }] if commands[0].success), "{outcomes:?}");
    assert_eq!(*commands.calls.borrow(), ["uv cache dir", "uv cache clean"]);
    assert_eq!(*commands.envs.borrow(), [vec![], vec!["UV_LOCK_TIMEOUT=10".to_owned()]]);
}

#[test]
fn a_move_to_the_trash_ignores_open_files_and_a_permanent_delete_does_not() {
    let home = Home::new();
    home.tree.write("nolock/.venv/pyvenv.cfg", "home = /usr/bin");
    home.tree.file("nolock/.venv/lib/x.py", 4_000);
    home.tree.file("Downloads/movie.mp4", 9_000);
    let venv = home.path("nolock/.venv");
    let movie = home.path("Downloads/movie.mp4");
    let review = home.listed(&venv, ActionKind::Trash, false);
    assert_eq!(review.class, RiskClass::Review);
    let picked = home.pick("Downloads/movie.mp4", "user.downloads");
    let player = FakeProcs(vec![holder(705, "python", &[&venv.join("lib/x.py")]), holder(706, "IINA", &[&movie])]);

    let permanent = ListedItem { action: ActionKind::Delete, permanent: true, ..review.clone() };
    assert_eq!(
        home.predict_and_clean(std::slice::from_ref(&permanent), &FakeCommands::default(), &player, &player),
        [in_use(705, "python")],
        "deleted for good: an open file counts"
    );

    // The path check still holds for a move to the Trash: a process working inside it.
    let working =
        FakeProcs(vec![ProcInfo { pid: 709, name: "python".into(), cwd: Some(venv.clone()), ..ProcInfo::default() }]);
    assert_eq!(
        home.predict_and_clean(std::slice::from_ref(&review), &FakeCommands::default(), &working, &working),
        [in_use(709, "python")],
        "its working directory is inside it"
    );

    let outcomes = home.predict_and_clean(&[review, picked], &FakeCommands::default(), &player, &player);
    assert_eq!(outcomes, [Outcome::Trashed, Outcome::Trashed]);
    assert!(!venv.exists() && !movie.exists());
}

#[test]
fn a_runtime_prunes_while_its_daemon_works_inside_its_finding() {
    let home = Home::new();
    home.tree.file("Library/Containers/com.docker.docker/Data/vms/0/Docker.raw", 50_000);
    let docker = home.path("Library/Containers/com.docker.docker");
    home.tree.write(DOCKER_CONTEXT, "{}");
    let item = home.listed(&docker, ActionKind::NativeOnly, false);
    let daemon = FakeProcs(vec![docker_backend(&docker)]);
    let commands = FakeCommands::default().with("docker", true, "").answer("docker", DOCKER_VERSION, true, "28.5.1\n");

    assert_eq!(home.dry_run(std::slice::from_ref(&item), &commands, &daemon), [Prediction::WouldRun]);
    assert!(commands.calls.borrow().is_empty(), "the dry run runs nothing: {:?}", commands.calls.borrow());
    let outcomes = home.predict_and_clean(&[item], &commands, &daemon, &daemon);
    let [Outcome::Native { commands: ran }] = &outcomes[..] else { panic!("{outcomes:?}") };
    assert_eq!(ran.len(), 2, "both prune commands");
    assert!(docker.join("Data/vms/0/Docker.raw").exists(), "the runtime's files are never removed by ddc");
}

#[test]
fn the_dry_run_predicts_a_docker_desktop_that_cannot_answer_without_running_anything() {
    let home = Home::new();
    home.tree.file("Library/Containers/com.docker.docker/Data/vms/0/Docker.raw", 50_000);
    let docker = home.path("Library/Containers/com.docker.docker");
    let item = home.listed(&docker, ActionKind::NativeOnly, false);
    let not_ready = |detail: &str| SkipReason::ToolNotReady { program: "docker".into(), detail: detail.into() };

    // No backend process: Docker Desktop is not running, and its probe fails as the clean runs it.
    let commands = FakeCommands::default().with("docker", true, "").answer("docker", DOCKER_VERSION, false, "");
    let quit = FakeProcs(vec![]);
    let down = not_ready("Docker Desktop is not running or not answering");
    assert_eq!(home.dry_run(std::slice::from_ref(&item), &commands, &quit), [Prediction::WouldSkip(down.clone())]);
    assert!(commands.calls.borrow().is_empty(), "the dry run runs nothing: {:?}", commands.calls.borrow());
    assert_eq!(home.predict_and_clean(std::slice::from_ref(&item), &commands, &quit, &quit), [Outcome::Skipped(down)]);
    assert_eq!(*commands.calls.borrow(), [format!("docker {DOCKER_VERSION}")], "the clean probed, no prune ran");

    // Running, but without the desktop-linux context.
    let commands = FakeCommands::default()
        .with("docker", true, "")
        .answer("docker", DOCKER_VERSION, false, "")
        .stderr_on("docker", DOCKER_VERSION, "context \"desktop-linux\" does not exist\n");
    let running = FakeProcs(vec![docker_backend(&docker)]);
    let missing = not_ready("the desktop-linux context does not exist");
    assert_eq!(
        home.predict_and_clean(&[item], &commands, &running, &running),
        [Outcome::Skipped(missing)],
        "predicted from the missing metadata file, said by the probe"
    );
}

#[test]
fn the_reason_names_the_app_a_holder_belongs_to() {
    let home = Home::new();
    let nm = home.node_modules();
    let item = home.listed(&nm, ActionKind::Delete, false);
    let helper = ProcInfo {
        open_files: vec![nm.join("dep/index.js")],
        ..app_process(
            708,
            "/Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper (Plugin).app/Contents/MacOS/Code Helper (Plugin)",
        )
    };
    let procs = FakeProcs(vec![helper]);

    assert_eq!(
        home.predict_and_clean(&[item], &FakeCommands::default(), &procs, &procs),
        [in_use(708, "Visual Studio Code")]
    );
}
