//! Story "Worktree findings and provenance" (epic DDC-003), its third step:
//! removal. A linked worktree goes through `git -C <main> worktree remove
//! <path>` alone, after its six conditions are checked again, and only
//! where git lists it; never `delete`, `trash`, `--force` or `prune` (ADR
//! "Agent footprint: worktrees, harness data and bounded consent for
//! agents", §2, and the plan-gate decisions 6 and 8). Real repositories
//! made with the `git` on `PATH`; the clean runs its own git through
//! `SharedCommands`, which records every call.

mod common;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use common::fakes::{FakeProcs, FakeTrash, Recorded, SharedCommands};
use common::{TreeBuilder, git};
use ddc_core::class::RiskClass;
use ddc_core::env::Env;
use ddc_core::exec::preflight::{self, Prediction, PreflightLine, PreflightPorts};
use ddc_core::exec::{
    self, CmdOutput, CommandRunner, Consent, ExecOptions, ExecReport, MemoryJournal, NATIVE_TIMEOUT, Outcome, Ports,
    SkipReason,
};
use ddc_core::git::{CLEARED, PROBE_CONFIG, PROBE_ENV};
use ddc_core::model::{Finding, RemovalKind, ScanReport};
use ddc_core::plan::{
    Action, ActionKind, DropReason, ListedItem, ListedPlan, Plan, PlanOptions, build_plan, plan_listed_checked,
};
use ddc_core::report::{self, Commands, Processes, ScanOptions};
use ddc_core::rules::{Catalog, worktree};
use ddc_core::scan::{Progress, StdReader, WalkOptions};

/// How long the fixtures have been idle when the scan looks at them.
const IDLE: Duration = Duration::from_secs(3 * 86_400 + 3_600);

struct Home {
    t: TreeBuilder,
    data: tempfile::TempDir,
    now: SystemTime,
}

impl Home {
    fn ready_after(test: &str, idle: Duration) -> Option<Self> {
        if common::unsupported(test) {
            return None;
        }
        common::skipped_without(test, git::available())?;
        Some(Self { t: TreeBuilder::new(), data: tempfile::tempdir().unwrap(), now: SystemTime::now() + idle })
    }

    fn ready(test: &str) -> Option<Self> {
        Self::ready_after(test, IDLE)
    }

    fn env(&self) -> Env {
        Env::with_paths(self.t.root(), self.data.path(), &self.data.path().join("logs"), self.now).unwrap()
    }

    fn catalog(&self) -> Catalog {
        Catalog::builtin(self.t.root())
    }

    fn opts(&self, commands: &Arc<SharedCommands>) -> ScanOptions {
        ScanOptions {
            walk: WalkOptions { threads: Some(4), ..Default::default() },
            processes: Processes::fixed(FakeProcs::new(vec![])),
            commands: Commands::shared(commands.clone()),
            ..Default::default()
        }
    }

    fn scan(&self) -> ScanReport {
        let env = self.env();
        let commands = Arc::new(SharedCommands::system(&env));
        report::scan(&env, &self.opts(&commands), &Progress::default()).unwrap()
    }

    fn finding<'a>(&self, r: &'a ScanReport, path: &Path) -> &'a Finding {
        r.findings.iter().find(|f| f.path == path).unwrap_or_else(|| {
            let all: Vec<_> = r.findings.iter().map(|f| (f.path.display().to_string(), f.rule.clone())).collect();
            panic!("no finding at {}: {all:?}", path.display())
        })
    }

    fn rel<'p>(&self, path: &'p Path) -> &'p str {
        path.strip_prefix(self.t.root()).unwrap().to_str().unwrap()
    }

    fn write(&self, path: &Path, text: &str) {
        self.t.write(self.rel(path), text);
    }

    fn trash(&self) -> FakeTrash {
        FakeTrash(self.data.path().join("FakeTrash"))
    }

    /// `plan` executed under `consent` with `commands`.
    fn execute(&self, plan: &Plan, consent: Consent, commands: &dyn CommandRunner) -> (ExecReport, MemoryJournal) {
        let mut journal = MemoryJournal(Vec::new());
        let trash = self.trash();
        let procs = FakeProcs::new(vec![]);
        let ports = Ports { commands, trash: &trash, procs: &procs, journal: &mut journal };
        let run = exec::execute(&self.env(), &self.catalog(), plan, &consent, ports, &ExecOptions::default()).unwrap();
        (run, journal)
    }

    /// `clean --yes --class rebuildable` of a fresh scan.
    fn clean_yes(&self, commands: &dyn CommandRunner) -> (Plan, ExecReport, MemoryJournal) {
        let plan = build_plan(&self.scan(), &self.catalog(), &PlanOptions::default());
        let (run, journal) = self.execute(&plan, yes(), commands);
        (plan, run, journal)
    }

    /// The person's list planned as `clean --items` plans it: a listed
    /// worktree's conditions already checked through `commands`' git.
    fn plan_listed(&self, items: &[ListedItem], commands: &dyn CommandRunner) -> ListedPlan {
        let env = self.env();
        let commands_arc = Arc::new(SharedCommands::system(&env));
        let (scanned, entries) = report::scan_with_entries(
            &env,
            &self.opts(&commands_arc),
            &Progress::default(),
            &report::volumes::SystemStatFs,
        )
        .unwrap();
        let rc = worktree::Recheck {
            commands,
            tool_dirs: env.tool_dirs(),
            timeout: ddc_core::git::PROBE_TIMEOUT,
            now: env.now(),
            min_age: report::DEFAULT_WORKTREE_MIN_AGE,
        };
        plan_listed_checked(&scanned, &entries, &self.catalog(), items, &FakeProcs::new(vec![]), &[], Some(&rc))
    }

    /// The dry run of a person's list.
    fn preflight(&self, items: &[ListedItem], commands: &dyn CommandRunner) -> Vec<PreflightLine> {
        let mut lines = Vec::new();
        let procs = FakeProcs::new(vec![]);
        let ports = PreflightPorts { commands, procs: &procs, reader: &StdReader };
        preflight::preflight(&self.env(), &self.catalog(), &[], &[], items, &ports, &mut |l| {
            lines.push(l.clone());
            Ok(())
        })
        .unwrap();
        lines
    }
}

fn yes() -> Consent {
    Consent::AssumeYes { classes: vec![RiskClass::Rebuildable] }
}

/// `proj`, its `.gitignore` and `uv.lock` committed, and a Claude Code
/// worktree of it on branch `worktree-<name>`.
fn project(h: &Home) -> (PathBuf, PathBuf) {
    let main = git::repo(&h.t, "proj");
    h.write(&main.join(".gitignore"), ".venv/\n.env\nstate/\n");
    h.write(&main.join("uv.lock"), "version = 1\n");
    git::git(&main, &["add", ".gitignore", "uv.lock"]);
    git::git(&main, &["commit", "-q", "-m", "ignore"]);
    let wt = agent_worktree(&main, "x");
    (main, wt)
}

fn agent_worktree(main: &Path, name: &str) -> PathBuf {
    git::worktree(main, &main.join(".claude/worktrees").join(name), &["-b", &format!("worktree-{name}")])
}

/// A `.venv` with `pyvenv.cfg` inside `dir`: an artifact of its own.
fn venv(h: &Home, dir: &Path) -> PathBuf {
    h.write(&dir.join(".venv/pyvenv.cfg"), "home = /usr/bin\n");
    h.t.file(&format!("{}/.venv/lib/site.py", h.rel(dir)), 30_000);
    dir.join(".venv")
}

fn outcome<'a>(run: &'a ExecReport, path: &Path) -> &'a Outcome {
    &run.items.iter().find(|i| i.path == path).unwrap_or_else(|| panic!("{path:?} not run: {:?}", run.items)).outcome
}

/// The finding at `path` as the app lists it.
fn listed(r: &ScanReport, path: &Path) -> ListedItem {
    let f = r.findings.iter().find(|f| f.path == path).unwrap_or_else(|| panic!("no finding at {path:?}"));
    let action = match f.removal {
        RemovalKind::Delete | RemovalKind::None => ActionKind::Delete,
        RemovalKind::Trash => ActionKind::Trash,
        RemovalKind::Native => ActionKind::Native,
        RemovalKind::NativeOnly => ActionKind::NativeOnly,
    };
    ListedItem {
        path: f.path.clone(),
        dev: f.dev,
        ino: f.ino,
        rule: f.rule.clone(),
        class: f.class,
        action,
        permanent: false,
    }
}

fn is_remove(call: &Recorded) -> bool {
    call.args.windows(2).any(|w| w == ["worktree", "remove"])
}

/// Every git call the clean made: under the probe contract, never `prune`
/// or `--force`; exactly one `worktree remove` per removed worktree.
fn assert_contract(commands: &SharedCommands, removes: usize) {
    let calls = commands.git_calls();
    assert!(!calls.is_empty());
    for call in &calls {
        assert_eq!(call.args.first().map(String::as_str), Some("--no-optional-locks"), "{:?}", call.args);
        for entry in PROBE_CONFIG {
            assert!(call.args.iter().any(|a| a == entry), "{entry}: {:?}", call.args);
        }
        for (k, v) in PROBE_ENV {
            assert!(call.env.contains(&(k.to_owned(), v.to_owned())), "{k}: {:?}", call.env);
        }
        assert_eq!(call.cleared.as_deref(), Some(CLEARED));
        assert!(!call.args.iter().any(|a| a == "prune" || a == "--force" || a == "-f"), "{:?}", call.args);
    }
    let removed: Vec<&Recorded> = calls.iter().filter(|c| is_remove(c)).collect();
    assert_eq!(removed.len(), removes, "{removed:?}");
    for call in removed {
        assert_eq!(call.timeout, NATIVE_TIMEOUT);
    }
}

/// The system's git, with some answers doctored.
struct Doctored {
    inner: SharedCommands,
    /// The worktree `worktree list --porcelain` leaves out.
    hide: Option<PathBuf>,
    /// `status` exits 128.
    status_fails: bool,
}

impl Doctored {
    fn new(env: &Env) -> Self {
        Self { inner: SharedCommands::system(env), hide: None, status_fails: false }
    }

    fn doctor(&self, args: &[String], out: io::Result<CmdOutput>) -> io::Result<CmdOutput> {
        if self.status_fails && args.iter().any(|a| a == "status") {
            return Ok(CmdOutput { success: false, code: Some(128), stdout: String::new(), stderr: "fatal".into() });
        }
        let mut out = out?;
        if let Some(hide) = &self.hide
            && args.windows(2).any(|w| w == ["worktree", "list"])
        {
            let line = format!("worktree {}", hide.display());
            out.stdout = out.stdout.split_inclusive("\n\n").filter(|b| b.lines().next() != Some(&line)).collect();
        }
        Ok(out)
    }
}

impl CommandRunner for Doctored {
    fn find(&self, program: &str) -> Option<PathBuf> {
        self.inner.find(program)
    }
    fn at(&self, exe: &Path) -> Option<PathBuf> {
        self.inner.at(exe)
    }
    fn run(&self, exe: &Path, args: &[String], env: &[(&str, &str)], timeout: Duration) -> io::Result<CmdOutput> {
        self.doctor(args, self.inner.run(exe, args, env, timeout))
    }
    fn run_cleared(
        &self,
        exe: &Path,
        args: &[String],
        cleared: &str,
        env: &[(&str, &str)],
        timeout: Duration,
    ) -> io::Result<CmdOutput> {
        self.doctor(args, self.inner.run_cleared(exe, args, cleared, env, timeout))
    }
}

/// Every file under `dir` with its bytes.
fn contents(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let path = e.unwrap().path();
            if fs::symlink_metadata(&path).unwrap().is_dir() {
                stack.push(path);
            } else {
                out.push((path.clone(), fs::read(&path).unwrap()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn a_selected_worktree_is_removed_through_git_alone() {
    let Some(h) = Home::ready("a_selected_worktree_is_removed_through_git_alone") else { return };
    let (main, wt) = project(&h);
    let other = agent_worktree(&main, "y");
    let other_admin = git::gitdir(&other);
    let plan = build_plan(&h.scan(), &h.catalog(), &PlanOptions::default());
    for path in [&wt, &other] {
        assert!(plan.items.iter().any(|i| &i.finding.path == path && i.selected), "{}", path.display());
    }
    // The second one is moved away before the clean.
    let moved = h.t.path("moved-y");
    fs::rename(&other, &moved).unwrap();
    let admin_before = contents(&other_admin);
    let commands = SharedCommands::system(&h.env());

    let (run, journal) = h.execute(&plan, yes(), &commands);

    let Outcome::Native { commands: ran } = outcome(&run, &wt) else { panic!("{:?}", outcome(&run, &wt)) };
    assert_eq!(ran.len(), 1);
    assert!(ran[0].success, "{:?}", ran[0]);
    assert!(ran[0].args.ends_with(&["worktree".into(), "remove".into(), wt.to_str().unwrap().into()]), "{ran:?}");
    assert!(matches!(outcome(&run, &other), Outcome::Skipped(_)), "{:?}", outcome(&run, &other));
    assert_contract(&commands, 1);
    assert!(!wt.exists());
    let list = git::git(&main, &["worktree", "list", "--porcelain"]);
    assert!(!list.contains(".claude/worktrees/x"), "{list}");
    assert!(!main.join(".git/worktrees/x").exists(), "its administrative folder went with it");
    assert_eq!(git::git(&main, &["branch", "--list", "worktree-x"]), "worktree-x", "its branch is kept");
    // The moved one's administrative folder is untouched: HEAD, index, reflog.
    assert_eq!(contents(&other_admin), admin_before);
    assert!(moved.join("README").exists());
    let line = journal.0.iter().find(|e| e.path == wt).expect("a journal line");
    assert_eq!((line.rule.as_str(), line.method), ("git.worktree", "native"));
    assert_eq!(&line.outcome, outcome(&run, &wt), "the journal carries the command and its output's tail");
}

#[test]
fn a_worktree_git_does_not_list_is_skipped_and_nothing_runs() {
    let Some(h) = Home::ready("a_worktree_git_does_not_list_is_skipped_and_nothing_runs") else { return };
    let (_, wt) = project(&h);
    let commands = Doctored { hide: Some(wt.clone()), ..Doctored::new(&h.env()) };

    let (_, run, _) = h.clean_yes(&commands);

    let reason = SkipReason::ToolNotReady { program: "git".into(), detail: worktree::NOT_LISTED.into() };
    assert_eq!(outcome(&run, &wt), &Outcome::Skipped(reason));
    assert!(!commands.inner.git_calls().iter().any(is_remove));
    assert!(wt.join("README").exists());
}

#[test]
fn delete_or_trash_on_a_worktree_is_not_allowed() {
    let Some(h) = Home::ready("delete_or_trash_on_a_worktree_is_not_allowed") else { return };
    let (_, wt) = project(&h);
    let plan = build_plan(&h.scan(), &h.catalog(), &PlanOptions::default());
    for action in [Action::Trash, Action::Delete { make_writable: false }] {
        let mut plan = plan.clone();
        plan.items.iter_mut().filter(|i| i.finding.path == wt).for_each(|i| i.action = action);
        let commands = SharedCommands::system(&h.env());

        let (run, _) = h.execute(&plan, yes(), &commands);

        assert_eq!(outcome(&run, &wt), &Outcome::Skipped(SkipReason::ActionNotAllowed), "{action:?}");
        assert!(commands.git_calls().is_empty(), "{action:?}");
        assert!(wt.join("README").exists());
    }
}

/// A worktree made dirty, given unique ignored content or given a commit
/// between the scan and the clean is kept, naming the condition.
#[test]
fn a_worktree_changed_since_the_scan_is_kept() {
    let Some(h) = Home::ready("a_worktree_changed_since_the_scan_is_kept") else { return };
    let (_, wt) = project(&h);
    let plan = build_plan(&h.scan(), &h.catalog(), &PlanOptions::default());
    let changes: [(&str, u8, &dyn Fn()); 3] = [
        ("an untracked file", 5, &|| h.write(&wt.join("notes.txt"), "draft")),
        ("unique ignored content", 5, &|| h.write(&wt.join("state/local-only.txt"), "mine")),
        ("a commit of its own", 6, &|| {
            git::git(&wt, &["add", "-f", "state/local-only.txt"]);
            git::git(&wt, &["commit", "-q", "-m", "mine"]);
        }),
    ];
    for (what, condition, change) in changes {
        change();
        let commands = SharedCommands::system(&h.env());

        let (run, _) = h.execute(&plan, yes(), &commands);

        match outcome(&run, &wt) {
            Outcome::Skipped(SkipReason::WorktreeKept { condition: c, .. }) => assert_eq!(*c, condition, "{what}"),
            other => panic!("{what}: {other:?}"),
        }
        assert!(!commands.git_calls().iter().any(is_remove), "{what}");
        assert!(wt.join("README").exists(), "{what}");
        if what == "an untracked file" {
            fs::rename(wt.join("notes.txt"), h.t.path("notes.txt")).unwrap();
        }
    }
}

#[test]
fn a_failing_git_at_the_clean_keeps_the_worktree() {
    let Some(h) = Home::ready("a_failing_git_at_the_clean_keeps_the_worktree") else { return };
    let (_, wt) = project(&h);
    let commands = Doctored { status_fails: true, ..Doctored::new(&h.env()) };
    let plan = build_plan(&h.scan(), &h.catalog(), &PlanOptions::default());

    let (run, _) = h.execute(&plan, yes(), &commands);

    match outcome(&run, &wt) {
        Outcome::Skipped(SkipReason::WorktreeKept { condition: 5, detail }) => {
            assert!(detail.contains("git could not tell"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
    assert!(wt.join("README").exists());
}

/// An item inside a selected worktree leaves the plan as `contained`, and
/// the worktree item holds its bytes: the plan counts them once. With the
/// worktree kept, the item is planned on its own.
#[test]
fn an_artifact_inside_a_selected_worktree_is_contained() {
    let Some(h) = Home::ready("an_artifact_inside_a_selected_worktree_is_contained") else { return };
    let (_, wt) = project(&h);
    let venv = venv(&h, &wt);
    let r = h.scan();
    let (f, inner) = (h.finding(&r, &wt).clone(), h.finding(&r, &venv).clone());
    assert!(f.selected, "{}", f.hint);

    let plan = build_plan(&r, &h.catalog(), &PlanOptions::default());

    assert!(!plan.items.iter().any(|i| i.finding.path == venv));
    let dropped = plan.dropped.iter().find(|d| d.finding.path == venv).expect("dropped");
    assert_eq!(dropped.reason, DropReason::Contained { worktree: wt.clone() });
    let item = plan.items.iter().find(|i| i.finding.path == wt).unwrap();
    assert_eq!(item.finding.bytes, f.bytes + inner.bytes);
    let total: u64 = plan.items.iter().filter(|i| i.selected).map(|i| i.finding.bytes).sum();
    let selected: u64 = r.findings.iter().filter(|f| f.selected && !f.ignored).map(|f| f.bytes).sum();
    assert_eq!(total, selected + inner.bytes * u64::from(!inner.selected));

    // Locked: kept, and the .venv is an item of its own again.
    fs::write(git::gitdir(&wt).join("locked"), "busy").unwrap();
    let r = h.scan();
    assert!(!h.finding(&r, &wt).selected);

    let plan = build_plan(&r, &h.catalog(), &PlanOptions::default());

    assert!(plan.items.iter().any(|i| i.finding.path == venv));
    assert!(plan.dropped.is_empty());
}

/// Decision 6: a person's list waives conditions 1 and 3, never the rest.
#[test]
fn a_listed_worktree_of_ones_own_is_removed_unless_it_holds_unique_content() {
    let Some(h) = Home::ready("a_listed_worktree_of_ones_own_is_removed_unless_it_holds_unique_content") else {
        return;
    };
    let (main, _) = project(&h);
    let own = git::worktree(&main, &h.t.path("own"), &["-b", "own"]);
    let held = git::worktree(&main, &h.t.path("held"), &["-b", "held"]);
    h.write(&held.join(".env"), "TOKEN=unique\n");
    let r = h.scan();
    let items = [listed(&r, &own), listed(&r, &held)];
    let commands = SharedCommands::system(&h.env());

    let dry = h.preflight(&items, &commands);

    let predicted = |path: &Path| {
        dry.iter()
            .find_map(|l| match l {
                PreflightLine::Item(i) if i.path == path => Some(i.prediction.clone()),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(predicted(&own), Prediction::WouldRun);
    assert!(
        matches!(predicted(&held), Prediction::WouldSkip(SkipReason::WorktreeKept { condition: 5, .. })),
        "{:?}",
        predicted(&held)
    );
    assert!(!commands.git_calls().iter().any(is_remove), "the dry run removes nothing");

    let plan = h.plan_listed(&items, &commands);

    let refused = plan.refused.iter().find(|r| r.item.path == held).expect("refused");
    assert!(matches!(&refused.reason, SkipReason::WorktreeKept { condition: 5, detail } if detail.contains(".env")));
    let (run, _) = h.execute(&plan.plan, Consent::Listed, &commands);
    assert!(matches!(outcome(&run, &own), Outcome::Native { .. }), "{:?}", outcome(&run, &own));
    assert!(!own.exists());
    assert_eq!(fs::read_to_string(held.join(".env")).unwrap(), "TOKEN=unique\n");
    assert_contract(&commands, 1);
}

#[test]
fn a_listed_agent_worktree_touched_an_hour_ago_is_removed() {
    let hour = Duration::from_secs(3_600);
    let Some(h) = Home::ready_after("a_listed_agent_worktree_touched_an_hour_ago_is_removed", hour) else { return };
    let (_, wt) = project(&h);
    let r = h.scan();
    assert!(!h.finding(&r, &wt).selected, "within the window");
    let commands = SharedCommands::system(&h.env());

    let plan = h.plan_listed(&[listed(&r, &wt)], &commands);
    let (run, _) = h.execute(&plan.plan, Consent::Listed, &commands);

    assert!(plan.refused.is_empty(), "{:?}", plan.refused);
    assert!(matches!(outcome(&run, &wt), Outcome::Native { .. }), "{:?}", outcome(&run, &wt));
    assert!(!wt.exists());
}

/// Decision 8: listing a worktree and the `.venv` inside it removes the
/// worktree and refuses the `.venv` as `inside-another-item`, in the dry
/// run and in the clean.
#[test]
fn an_item_listed_inside_a_listed_worktree_goes_with_it() {
    let Some(h) = Home::ready("an_item_listed_inside_a_listed_worktree_goes_with_it") else { return };
    let (_, wt) = project(&h);
    let venv = venv(&h, &wt);
    let r = h.scan();
    let items = [listed(&r, &wt), listed(&r, &venv)];
    let commands = SharedCommands::system(&h.env());

    let dry = h.preflight(&items, &commands);

    let venv_line = dry.iter().find_map(|l| match l {
        PreflightLine::Item(i) if i.path == venv => Some(i.prediction.clone()),
        _ => None,
    });
    assert_eq!(venv_line, Some(Prediction::WouldSkip(SkipReason::InsideAnotherItem)));

    let plan = h.plan_listed(&items, &commands);
    let (run, _) = h.execute(&plan.plan, Consent::Listed, &commands);

    let refused = plan.refused.iter().find(|r| r.item.path == venv).expect("refused");
    assert_eq!(refused.reason, SkipReason::InsideAnotherItem);
    assert!(matches!(outcome(&run, &wt), Outcome::Native { .. }), "{:?}", outcome(&run, &wt));
    assert!(!wt.exists());
    assert!(!h.trash().0.exists(), "nothing went to the Trash");
}

/// An orphan's repository is gone: a listed one goes to the Trash, or is
/// deleted with `--permanent`; no git runs for it.
#[test]
fn a_listed_orphan_goes_to_the_trash_or_is_deleted() {
    let Some(h) = Home::ready("a_listed_orphan_goes_to_the_trash_or_is_deleted") else { return };
    let (main, _) = project(&h);
    let a = git::worktree(&main, &h.t.path("a"), &["-b", "a"]);
    let b = git::worktree(&main, &h.t.path("b"), &["-b", "b"]);
    // The repository's administrative folders are gone: moved out of it.
    fs::rename(main.join(".git/worktrees"), h.t.path("gone")).unwrap();
    let r = h.scan();
    for path in [&a, &b] {
        assert_eq!(h.finding(&r, path).rule, worktree::ORPHAN_RULE);
    }
    let mut permanent = listed(&r, &b);
    permanent.action = ActionKind::Delete;
    permanent.permanent = true;
    let items = [listed(&r, &a), permanent];
    let commands = SharedCommands::system(&h.env());

    let plan = h.plan_listed(&items, &commands);
    let (run, _) = h.execute(&plan.plan, Consent::Listed, &commands);

    assert!(plan.refused.is_empty(), "{:?}", plan.refused);
    assert_eq!(outcome(&run, &a), &Outcome::Trashed);
    assert!(outcome(&run, &b) == &Outcome::Deleted, "{:?}", outcome(&run, &b));
    assert!(!a.exists() && !b.exists());
    assert!(h.trash().0.read_dir().unwrap().next().is_some());
    assert!(!commands.git_calls().iter().any(is_remove));
}
