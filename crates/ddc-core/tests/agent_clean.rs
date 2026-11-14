//! Story "Clean: --agent bounded consent" (epic DDC-003), the core: what a
//! coding agent's plan considers inside its project's checkouts, what goes,
//! what is left for a person, and the executor's own check under
//! `Consent::Agent`. Real repositories made with the `git` on `PATH`
//! (`common::git`), walked as `ddc agent clean` walks them.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use common::fakes::{FakeProcs, FakeTrash, SharedCommands};
use common::{TreeBuilder, git};
use ddc_core::class::{Category, RiskClass};
use ddc_core::env::Env;
use ddc_core::exec::report::{AgentPart, CleanReport};
use ddc_core::exec::{
    self, Consent, ConsentKind, ExecOptions, ExecReport, MemoryJournal, Outcome, Ports, ProcInfo, ProcessTable,
    SkipReason,
};
use ddc_core::footprint::{self, Scope};
use ddc_core::git::PROBE_TIMEOUT;
use ddc_core::model::{RemovalKind, ScanReport};
use ddc_core::plan::{
    Action, ActionKind, AgentBound, AgentPlan, AgentVerdict, ListedItem, Plan, PlanItem, PlanOptions, agent_may_remove,
    build_plan, plan_agent,
};
use ddc_core::report::volumes::SystemStatFs;
use ddc_core::report::{self, Commands, Processes, ScanOptions, recheck};
use ddc_core::rules::Catalog;
use ddc_core::scan::{Progress, WalkOptions};

/// How long the fixtures have been idle when the agent looks at them: past
/// the worktree window (2 days) and the projects' (14), so the `.venv` and
/// the worktree are selected by default.
const IDLE: Duration = Duration::from_secs(15 * 86_400 + 3_600);

struct Home {
    t: TreeBuilder,
    data: tempfile::TempDir,
    now: SystemTime,
}

impl Home {
    fn ready(test: &str) -> Option<Self> {
        if common::unsupported(test) {
            return None;
        }
        common::skipped_without(test, git::available())?;
        Some(Self { t: TreeBuilder::new(), data: tempfile::tempdir().unwrap(), now: SystemTime::now() + IDLE })
    }

    fn env(&self) -> Env {
        Env::with_paths(self.t.root(), self.data.path(), &self.data.path().join("logs"), self.now).unwrap()
    }

    fn write(&self, path: &Path, text: &str) {
        self.t.write(path.strip_prefix(self.t.root()).unwrap().to_str().unwrap(), text);
    }

    fn opts(&self, commands: &Arc<SharedCommands>) -> ScanOptions {
        ScanOptions {
            walk: WalkOptions { threads: Some(4), ..Default::default() },
            processes: Processes::fixed(FakeProcs::new(vec![])),
            commands: Commands::shared(commands.clone()),
            ..Default::default()
        }
    }

    /// A whole scan of the home folder, as `ddc scan` makes it.
    fn scan(&self) -> ScanReport {
        let env = self.env();
        let commands = Arc::new(SharedCommands::system(&env));
        report::scan(&env, &self.opts(&commands), &Progress::default()).unwrap()
    }

    /// The walk `ddc agent clean` makes from `root`, and the bound it gives.
    fn walk(&self, root: &Path) -> (ScanReport, AgentBound) {
        let env = self.env();
        let commands = Arc::new(SharedCommands::system(&env));
        let scope = Scope::resolve(root, env.home(), &*commands, &env.tool_dirs(), PROBE_TIMEOUT).unwrap();
        let opts = footprint::scan_options(&scope, self.opts(&commands));
        let report = recheck::scoped(&env, &opts, &scope.reach(), &Progress::default(), &SystemStatFs, None).unwrap();
        let bound = AgentBound::new(&report, &scope.main, scope.bare, &scope.checkouts, env.home());
        (report, bound)
    }

    fn plan(&self, r: &ScanReport, bound: &AgentBound, listed: Option<&[ListedItem]>, opt_in: bool) -> AgentPlan {
        plan_agent(r, &Catalog::for_home(self.t.root()), bound, listed, opt_in)
    }

    fn run(&self, plan: &Plan, bound: &AgentBound, procs: &dyn ProcessTable) -> (ExecReport, MemoryJournal) {
        let env = self.env();
        let commands = SharedCommands::system(&env);
        let trash = FakeTrash(self.data.path().join("FakeTrash"));
        let mut journal = MemoryJournal::default();
        let consent = Consent::Agent { bound: bound.clone() };
        let ports = Ports { commands: &commands, trash: &trash, procs, journal: &mut journal };
        let report =
            exec::execute(&env, &Catalog::for_home(self.t.root()), plan, &consent, ports, &ExecOptions::default())
                .unwrap();
        (report, journal)
    }
}

/// `proj`: the main checkout's `pkg/__pycache__` and `.pytest_cache`
/// (`safe`) and `.venv` (`rebuildable`), and a harness worktree `a`, clean
/// and idle, with a `__pycache__` of its own. Beside it, `other` with its
/// own `__pycache__`: never this project's.
struct Proj {
    main: PathBuf,
    cache: PathBuf,
    tool_cache: PathBuf,
    venv: PathBuf,
    wt: PathBuf,
    wt_cache: PathBuf,
    other_cache: PathBuf,
}

fn project(h: &Home) -> Proj {
    let main = git::repo(&h.t, "proj");
    h.write(&main.join(".gitignore"), ".venv/\n__pycache__/\n.pytest_cache/\n");
    h.write(&main.join("uv.lock"), "version = 1\n");
    git::git(&main, &["add", ".gitignore", "uv.lock"]);
    git::git(&main, &["commit", "-q", "-m", "ignore"]);
    let wt = git::worktree(&main, &main.join(".claude/worktrees/a"), &["-b", "worktree-a"]);
    let pyc = "pkg/__pycache__/m.cpython-312.pyc";
    h.write(&main.join(pyc), &"p".repeat(20_000));
    h.write(&main.join(".pytest_cache/v/cache/nodeids"), &"t".repeat(5_000));
    h.write(&main.join(".venv/pyvenv.cfg"), "home = /usr/bin\n");
    h.write(&main.join(".venv/lib/site.py"), &"x".repeat(50_000));
    h.write(&wt.join(pyc), &"q".repeat(10_000));
    let other = h.t.root().join("other");
    h.write(&other.join(pyc), &"o".repeat(10_000));
    Proj {
        cache: main.join("pkg/__pycache__"),
        tool_cache: main.join(".pytest_cache"),
        venv: main.join(".venv"),
        wt_cache: wt.join("pkg/__pycache__"),
        other_cache: other.join("pkg/__pycache__"),
        main,
        wt,
    }
}

fn verdict<'a>(plan: &'a AgentPlan, path: &Path) -> &'a AgentVerdict {
    &plan.items.iter().find(|i| i.item.path == path).unwrap_or_else(|| panic!("{path:?} not considered")).verdict
}

fn for_person(plan: &AgentPlan) -> Vec<PathBuf> {
    plan.for_person.iter().map(|i| i.path.clone()).collect()
}

fn outcome<'a>(r: &'a ExecReport, path: &Path) -> &'a Outcome {
    &r.items.iter().find(|i| i.path == path).unwrap_or_else(|| panic!("{path:?} not executed: {:#?}", r.items)).outcome
}

/// A finding of the walk as a person's item list names it.
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

#[test]
fn with_the_opt_in_on_an_agent_removes_safe_items_and_lists_the_rest_for_a_person() {
    let Some(h) = Home::ready("with_the_opt_in_on_an_agent_removes_safe_items_and_lists_the_rest_for_a_person") else {
        return;
    };
    let p = project(&h);

    let (r, bound) = h.walk(&p.wt);
    assert_eq!(bound.project, p.main, "a worktree's clean is its main checkout's project");
    assert_eq!(bound.checkouts, [p.main.clone(), p.wt.clone()]);
    assert!(bound.left_out.is_empty(), "{:?}", bound.left_out);
    let plan = h.plan(&r, &bound, None, true);

    for path in [&p.cache, &p.tool_cache, &p.wt_cache] {
        assert_eq!(verdict(&plan, path), &AgentVerdict::Remove, "{path:?}");
    }
    for path in [&p.venv, &p.wt] {
        assert_eq!(verdict(&plan, path), &AgentVerdict::Skip(SkipReason::NotConsented), "{path:?}");
    }
    assert!(!plan.items.iter().any(|i| i.item.path == p.other_cache), "another project is never considered");
    let paths: Vec<&PathBuf> = plan.items.iter().map(|i| &i.item.path).collect();
    assert!(paths.is_sorted(), "by path: {paths:?}");
    assert_eq!(for_person(&plan), [p.wt.clone(), p.venv.clone()], "the worktree and the .venv, nothing inside it");
    assert_eq!(plan.plan.items.len(), 3);

    let (run, journal) = h.run(&plan.plan, &bound, &FakeProcs::new(vec![]));

    for path in [&p.cache, &p.tool_cache, &p.wt_cache] {
        assert_eq!(outcome(&run, path), &Outcome::Deleted, "{path:?}");
        assert!(!path.exists(), "{path:?}");
    }
    assert!(p.venv.exists() && p.wt.exists() && p.other_cache.exists());
    assert_eq!(journal.0.len(), 3);
    for line in &journal.0 {
        assert_eq!(line.consent, Some(ConsentKind::Agent), "{:?}", line.path);
        assert_eq!(line.project.as_deref(), Some(p.main.as_path()), "{:?}", line.path);
    }

    let report = CleanReport::for_agent(&plan.items, &plan.for_person, Some(&run), "journal".into(), part(&p, true));
    assert_eq!(report.consent, Some(ConsentKind::Agent));
    assert_eq!(report.project.as_deref(), Some(p.main.as_path()));
    assert_eq!(report.agents_clean_safe, Some(true));
    assert!(!report.dry_run);
    assert_eq!(report.items.len(), plan.items.len(), "every considered item, in the plan's order");
    let item = |path: &Path| report.items.iter().find(|i| i.path == path).unwrap();
    assert_eq!(item(&p.cache).outcome, Outcome::Deleted);
    assert!(item(&p.cache).bytes.is_some_and(|b| b > 0));
    assert_eq!(item(&p.venv).outcome, Outcome::Skipped(SkipReason::NotConsented));
    let listed = report.for_person.as_ref().expect("for the person");
    assert_eq!(listed.items, plan.for_person);
}

fn part(p: &Proj, opt_in: bool) -> AgentPart {
    AgentPart {
        project: Some(p.main.clone()),
        agents_clean_safe: opt_in,
        dry_run: false,
        settings_error: None,
        left_out: Vec::new(),
    }
}

#[test]
fn with_the_opt_in_off_nothing_goes_and_the_safe_items_are_listed_for_a_person() {
    let Some(h) = Home::ready("with_the_opt_in_off_nothing_goes_and_the_safe_items_are_listed_for_a_person") else {
        return;
    };
    let p = project(&h);

    let (r, bound) = h.walk(&p.main);
    let plan = h.plan(&r, &bound, None, false);

    assert!(plan.plan.items.is_empty(), "{:?}", plan.plan.items);
    for path in [&p.cache, &p.tool_cache, &p.wt_cache] {
        assert_eq!(verdict(&plan, path), &AgentVerdict::Skip(SkipReason::AgentOptInOff), "{path:?}");
    }
    assert_eq!(verdict(&plan, &p.venv), &AgentVerdict::Skip(SkipReason::NotConsented));
    assert_eq!(
        for_person(&plan),
        [p.wt.clone(), p.tool_cache.clone(), p.venv.clone(), p.cache.clone()],
        "the opt-in's items too; the worktree's own cache goes with the worktree"
    );

    // Nothing ran: the report names each skip, and `not-run` for nothing.
    let dry = AgentPart { dry_run: true, ..part(&p, false) };
    let report = CleanReport::for_agent(&plan.items, &plan.for_person, None, "journal".into(), dry);
    assert!(report.dry_run);
    assert_eq!(report.agents_clean_safe, Some(false));
    assert!(report.items.iter().all(|i| matches!(i.outcome, Outcome::Skipped(_))), "{:?}", report.items);
    // Under the opt-in, a dry run's removals are `not-run`.
    let on = h.plan(&r, &bound, None, true);
    let report =
        CleanReport::for_agent(&on.items, &[], None, "journal".into(), AgentPart { dry_run: true, ..part(&p, true) });
    assert_eq!(report.items.iter().find(|i| i.path == p.cache).unwrap().outcome, Outcome::NotRun);
    assert!(report.for_person.is_none(), "absent when empty");
}

/// A list the walk does not bear out removes nothing: an item listed with
/// another action, one the walk no longer finds, and one under a rule this
/// catalog does not have.
#[test]
fn a_stale_list_item_is_skipped_with_why() {
    let Some(h) = Home::ready("a_stale_list_item_is_skipped_with_why") else { return };
    let p = project(&h);
    let (r, bound) = h.walk(&p.main);
    let trash = ListedItem { action: ActionKind::Trash, ..listed(&r, &p.cache) };
    let gone = ListedItem { path: p.main.join("gone/__pycache__"), ..listed(&r, &p.cache) };
    let unknown = ListedItem { path: p.main.join("lib/__pycache__"), rule: "nope.rule".into(), ..listed(&r, &p.cache) };

    let plan = h.plan(&r, &bound, Some(&[trash, gone, unknown]), true);

    let verdicts: Vec<&AgentVerdict> = plan.items.iter().map(|i| &i.verdict).collect();
    assert_eq!(
        verdicts,
        [
            &AgentVerdict::Skip(SkipReason::ChangedSinceScan),
            &AgentVerdict::Skip(SkipReason::ChangedSinceScan),
            &AgentVerdict::Skip(SkipReason::RuleMismatch),
        ]
    );
    assert!(plan.plan.items.is_empty() && plan.for_person.is_empty());
}

#[test]
fn a_list_filters_the_agents_plan_and_never_widens_it() {
    let Some(h) = Home::ready("a_list_filters_the_agents_plan_and_never_widens_it") else { return };
    let p = project(&h);
    let (r, _) = h.walk(&p.main);
    let other = ListedItem {
        path: p.other_cache.clone(),
        dev: 0,
        ino: 0,
        rule: "python.pycache".into(),
        class: RiskClass::Safe,
        action: ActionKind::Delete,
        permanent: false,
    };
    let list = [listed(&r, &p.cache), listed(&r, &p.venv), other, listed(&r, &p.tool_cache)];

    // Kept since the person's list was made: the fresh walk says so.
    h.write(&p.tool_cache.join(".ddc-keep"), "");
    let (r, bound) = h.walk(&p.main);

    let plan = h.plan(&r, &bound, Some(&list), true);

    let verdicts: Vec<(&Path, &AgentVerdict)> =
        plan.items.iter().map(|i| (i.item.path.as_path(), &i.verdict)).collect();
    assert_eq!(
        verdicts,
        [
            (p.cache.as_path(), &AgentVerdict::Remove),
            (p.venv.as_path(), &AgentVerdict::Skip(SkipReason::NotConsented)),
            (p.other_cache.as_path(), &AgentVerdict::Skip(SkipReason::NotConsented)),
            (p.tool_cache.as_path(), &AgentVerdict::Skip(SkipReason::Kept)),
        ],
        "in the list's order"
    );
    assert_eq!(for_person(&plan), std::slice::from_ref(&p.venv), "only listed items");
    let planned: Vec<&PathBuf> = plan.plan.items.iter().map(|i| &i.finding.path).collect();
    assert_eq!(planned, [&p.cache]);
}

/// The executor asks the agent's rule again: a person's plan run under
/// `Consent::Agent` removes only the safe items inside the bound.
#[test]
fn the_executor_refuses_what_an_agent_may_not_remove_and_journals_it() {
    let Some(h) = Home::ready("the_executor_refuses_what_an_agent_may_not_remove_and_journals_it") else { return };
    let p = project(&h);
    let (r, bound) = h.walk(&p.main);
    let plan = build_plan(&r, &Catalog::for_home(h.t.root()), &PlanOptions::default());
    assert!(plan.items.iter().any(|i| i.finding.path == p.venv && i.default_selected), "the person's plan has it");

    let (run, journal) = h.run(&plan, &bound, &FakeProcs::new(vec![]));

    assert_eq!(outcome(&run, &p.venv), &Outcome::Skipped(SkipReason::NotConsented));
    assert_eq!(outcome(&run, &p.cache), &Outcome::Deleted);
    assert!(p.venv.exists() && !p.cache.exists());
    let venv_line = journal.0.iter().find(|l| l.path == p.venv).expect("the refusal is journaled");
    assert_eq!(venv_line.outcome, Outcome::Skipped(SkipReason::NotConsented));
    assert_eq!(venv_line.consent, Some(ConsentKind::Agent));
}

/// What the plan never hands it, the executor refuses on its own: an item
/// outside the bound, a `safe` one of another category, and one moved to
/// the Trash rather than deleted.
#[test]
fn the_executor_refuses_an_item_outside_the_agents_rule() {
    let Some(h) = Home::ready("the_executor_refuses_an_item_outside_the_agents_rule") else { return };
    let p = project(&h);
    let (r, bound) = h.walk(&p.main);
    let found = |path: &Path| r.findings.iter().find(|f| f.path == path).unwrap().clone();
    let delete = Action::Delete { make_writable: false };
    let outside = ddc_core::model::Finding { path: p.other_cache.clone(), ..found(&p.cache) };
    let other_category = ddc_core::model::Finding { category: Category::GenericCache, ..found(&p.tool_cache) };
    assert!(agent_may_remove(&found(&p.cache), &delete, &bound));
    assert!(!agent_may_remove(&found(&p.cache), &Action::Trash, &bound));
    let item = |finding, action| PlanItem { finding, action, default_selected: true, selected: true };
    let plan = Plan {
        roots: r.scan_roots.clone(),
        items: vec![item(outside, delete), item(other_category, delete), item(found(&p.wt_cache), Action::Trash)],
        dropped: Vec::new(),
    };

    let (run, journal) = h.run(&plan, &bound, &FakeProcs::new(vec![]));

    for path in [&p.other_cache, &p.tool_cache, &p.wt_cache] {
        assert_eq!(outcome(&run, path), &Outcome::Skipped(SkipReason::NotConsented), "{path:?}");
        assert!(path.exists(), "{path:?}");
    }
    assert_eq!(journal.0.len(), 3, "each refusal journaled");
}

/// Every re-check of a person's clean holds under an agent's: a cache
/// changed since the walk, one a process works in, and one kept after it.
#[test]
fn an_agents_clean_re_checks_each_item_as_a_persons_does() {
    let Some(h) = Home::ready("an_agents_clean_re_checks_each_item_as_a_persons_does") else { return };
    let p = project(&h);
    let (r, bound) = h.walk(&p.main);
    let plan = h.plan(&r, &bound, None, true);
    std::fs::rename(&p.cache, p.main.join("pkg/old_cache")).unwrap();
    h.write(&p.cache.join("fresh.pyc"), "new");
    h.write(&p.wt_cache.join(".ddc-keep"), "");
    let procs = FakeProcs::new(vec![ProcInfo {
        pid: 711,
        name: "pytest".into(),
        cwd: Some(p.tool_cache.join("v")),
        ..ProcInfo::default()
    }]);

    let (run, _) = h.run(&plan.plan, &bound, &procs);

    assert_eq!(outcome(&run, &p.cache), &Outcome::Skipped(SkipReason::ChangedSinceScan));
    assert!(matches!(outcome(&run, &p.tool_cache), Outcome::Skipped(SkipReason::InUse { pid: 711, .. })));
    assert_eq!(outcome(&run, &p.wt_cache), &Outcome::Skipped(SkipReason::Kept));
    assert!(p.cache.join("fresh.pyc").exists() && p.tool_cache.exists() && p.wt_cache.exists());
}

/// A checkout git lists that the walk does not recognise as this project's
/// worktree, or one at or above the home folder, leaves the bound with
/// why; the run goes on without it.
#[test]
fn a_checkout_the_walk_does_not_vouch_for_leaves_the_bound() {
    let Some(h) = Home::ready("a_checkout_the_walk_does_not_vouch_for_leaves_the_bound") else { return };
    let p = project(&h);
    let (r, _) = h.walk(&p.main);
    let stray = h.t.root().join("other");
    let home = h.t.root().to_path_buf();

    let bound = AgentBound::new(&r, &p.main, false, &[p.main.clone(), stray.clone()], &home);

    assert_eq!(bound.checkouts, std::slice::from_ref(&p.main));
    let left: Vec<&PathBuf> = bound.left_out.iter().map(|(path, _)| path).collect();
    assert_eq!(left, [&stray]);
    assert!(bound.left_out[0].1.contains("did not find it"), "{:?}", bound.left_out[0]);
    assert!(!bound.contains(&p.other_cache), "nothing under a stray checkout");
    assert!(!bound.contains(&p.main), "a checkout itself is never inside");
    assert!(bound.contains(&p.cache));

    // A worktree of another project, which a whole scan finds: not this one's.
    let elsewhere = git::repo(&h.t, "elsewhere");
    h.write(&elsewhere.join("README"), "x\n");
    git::git(&elsewhere, &["add", "README"]);
    git::git(&elsewhere, &["commit", "-q", "-m", "init"]);
    let foreign_wt = git::worktree(&elsewhere, &h.t.root().join("elsewhere-wt"), &["-b", "wt"]);
    let full = h.scan();
    let bound = AgentBound::new(&full, &p.main, false, &[p.main.clone(), p.wt.clone(), foreign_wt.clone()], &home);
    assert_eq!(bound.checkouts, [p.main.clone(), p.wt.clone()], "{:?}", bound.left_out);
    assert_eq!(bound.left_out.len(), 1);

    // A safe item that holds a checkout git lists is never the agent's.
    let held = p.tool_cache.join("wt");
    h.write(&held.join("x"), "");
    let bound = AgentBound::new(&r, &p.main, false, &[p.main.clone(), held], &home);
    let cache = r.findings.iter().find(|f| f.path == p.tool_cache).unwrap();
    assert!(!agent_may_remove(cache, &Action::Delete { make_writable: false }, &bound));

    // The home folder as the main checkout: at the home folder.
    let bound = AgentBound::new(&r, &home, false, std::slice::from_ref(&home), &home);
    assert!(bound.checkouts.is_empty(), "{:?}", bound.checkouts);
    assert!(bound.left_out[0].1.contains("home folder"), "{:?}", bound.left_out[0]);
    assert!(!bound.contains(&p.cache));

    // The home folder through a symlink is still the home folder.
    #[cfg(unix)]
    {
        let link = h.data.path().join("home-link");
        std::os::unix::fs::symlink(&home, &link).unwrap();
        let bound = AgentBound::new(&r, &home, false, std::slice::from_ref(&home), &link);
        assert!(bound.checkouts.is_empty(), "{:?}", bound.checkouts);
        // A checkout whose folder is another account's.
        let foreign = PathBuf::from("/usr/ddc-agent-bound-checkout");
        let bound = AgentBound::new(&r, &foreign, false, std::slice::from_ref(&foreign), &home);
        assert_eq!(bound.left_out.len(), 1, "{:?}", bound.left_out);
        assert!(bound.left_out[0].1.contains("another account"), "{:?}", bound.left_out[0]);
    }
}
