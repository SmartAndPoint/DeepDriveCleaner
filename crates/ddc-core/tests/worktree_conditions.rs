//! Story "Worktree findings and provenance" (epic DDC-003), its second step:
//! the six conditions under which a coding agent's linked worktree is
//! `rebuildable` and selected, and the git probe contract they run under
//! (ADR "Agent footprint: worktrees, harness data and bounded consent for
//! agents", §2 and its amendment of 2026-11-01). Read only: removal comes
//! with the next step. Real repositories made with the `git` on `PATH`
//! (`common::git`); the scan runs its own git through `SharedCommands`,
//! which records every call.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use common::fakes::{FakeProcs, Recorded, SharedCommands};
use common::{TreeBuilder, git};
use ddc_core::class::RiskClass;
use ddc_core::env::Env;
use ddc_core::exec::{ProcInfo, SystemCommands};
use ddc_core::git::{CLEARED, Git, PROBE_CONFIG, PROBE_ENV, PROBE_TIMEOUT};
use ddc_core::model::{Finding, IgnoredContent, NotSelected, RemovalKind, ScanReport, WorktreeProbe, WorktreeState};
use ddc_core::plan::{PlanOptions, build_plan};
use ddc_core::report::{self, Commands, Processes, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::scan::{Progress, WalkOptions};

/// How long the fixtures have been idle when the scan looks at them.
const IDLE: Duration = Duration::from_secs(3 * 86_400 + 3_600);

struct Home {
    t: TreeBuilder,
    data: tempfile::TempDir,
    now: SystemTime,
}

impl Home {
    /// A home whose scans run `idle` after the fixtures are made, or `None`
    /// after printing why the test skips: the scanner is not ported here, or
    /// no git runs.
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

    fn opts(&self, commands: &Arc<SharedCommands>) -> ScanOptions {
        ScanOptions {
            walk: WalkOptions { threads: Some(4), ..Default::default() },
            processes: Processes::fixed(FakeProcs::new(vec![])),
            commands: Commands::shared(commands.clone()),
            ..Default::default()
        }
    }

    fn scan_with(&self, env: &Env, opts: &ScanOptions) -> ScanReport {
        report::scan(env, opts, &Progress::default()).unwrap()
    }

    /// A scan with the system's tools, every call recorded.
    fn scan(&self) -> (ScanReport, Arc<SharedCommands>) {
        let env = self.env();
        let commands = Arc::new(SharedCommands::system(&env));
        (self.scan_with(&env, &self.opts(&commands)), commands)
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
}

/// `proj`, its `.gitignore` and `uv.lock` committed, and a Claude Code
/// worktree of it on branch `worktree-x`.
fn project(h: &Home) -> (PathBuf, PathBuf) {
    project_with(h, "proj", true)
}

fn project_with(h: &Home, rel: &str, lock: bool) -> (PathBuf, PathBuf) {
    let main = git::repo(&h.t, rel);
    h.write(&main.join(".gitignore"), ".venv/\n.env\nstate/\n");
    let mut add = vec!["add", ".gitignore"];
    if lock {
        h.write(&main.join("uv.lock"), "version = 1\n");
        add.push("uv.lock");
    }
    git::git(&main, &add);
    git::git(&main, &["commit", "-q", "-m", "ignore"]);
    let wt = git::worktree(&main, &main.join(".claude/worktrees/x"), &["-b", "worktree-x"]);
    (main, wt)
}

/// A commit of a new file in `dir`.
fn commit(h: &Home, dir: &Path, name: &str) -> String {
    h.write(&dir.join(name), name);
    git::git(dir, &["add", name]);
    git::git(dir, &["commit", "-q", "-m", name]);
    git::git(dir, &["rev-parse", "HEAD"])
}

fn state(f: &Finding) -> &WorktreeState {
    f.worktree.as_ref().expect("a worktree state")
}

/// It holds all six conditions: `rebuildable` and selected, removed through
/// git; its hint names the branch the removal keeps.
fn eligible(f: &Finding) {
    assert_eq!((f.class, f.selected, f.not_selected), (RiskClass::Rebuildable, true, None), "{}", f.hint);
    assert_eq!(f.removal, RemovalKind::Native);
    assert_eq!(state(f).probe, WorktreeProbe::Known);
    assert!(f.hint.contains("that nothing needs any more"), "{}", f.hint);
    let kept = match &state(f).branch {
        Some(branch) => format!("branch {branch} is kept, git branch -d to drop it"),
        None => "its HEAD is detached and no other ref loses a commit".to_owned(),
    };
    assert!(f.hint.contains(&kept), "{kept:?} in {}", f.hint);
}

fn kept(f: &Finding, says: &str) {
    assert_eq!((f.class, f.selected, f.not_selected), (RiskClass::Review, false, Some(NotSelected::Review)));
    assert!(f.hint.contains("; kept: ") && f.hint.contains(says), "{says:?} in {}", f.hint);
}

/// Each probe ran under the contract: no optional lock, every override,
/// both variables, the inherited `GIT_*` cleared.
fn under_contract(call: &Recorded) {
    assert_eq!(call.args.first().map(String::as_str), Some("--no-optional-locks"), "{:?}", call.args);
    for entry in PROBE_CONFIG {
        let at = call.args.iter().position(|a| a == entry).unwrap_or_else(|| panic!("{entry}: {:?}", call.args));
        assert_eq!(call.args[at - 1], "-c");
    }
    for (k, v) in PROBE_ENV {
        assert!(call.env.contains(&(k.to_owned(), v.to_owned())), "{k}: {:?}", call.env);
    }
    assert_eq!(call.cleared.as_deref(), Some(CLEARED));
    assert_eq!(call.timeout, PROBE_TIMEOUT);
}

#[test]
fn a_dead_claude_code_worktree_is_rebuildable_and_selected() {
    let Some(h) = Home::ready("a_dead_claude_code_worktree_is_rebuildable_and_selected") else { return };
    let (main, wt) = project(&h);

    let (r, commands) = h.scan();

    let f = h.finding(&r, &wt);
    eligible(f);
    let s = state(f);
    assert_eq!((s.locked, s.in_use, s.idle_days), (Some(false), Some(false), Some(3)));
    assert_eq!((s.submodules, s.clean, s.own_commits, &s.ignored_content), (Some(false), Some(true), Some(0), &None));
    assert_eq!(s.branch.as_deref(), Some("worktree-x"));
    assert!(f.hint.contains("branch worktree-x is kept, git branch -d to drop it"), "{}", f.hint);
    assert!(f.hint.starts_with(&format!("a linked worktree of {} that nothing needs", main.display())), "{}", f.hint);
    assert_eq!(f.project.as_ref().unwrap().idle_days, Some(3));
    let calls = commands.git_calls();
    assert!(!calls.is_empty());
    calls.iter().for_each(under_contract);
    assert!(calls.iter().all(|c| !c.args.iter().any(|a| a == "--all" || a == "--branches")), "{calls:?}");
}

/// The probe contract: a repository's `core.fsmonitor` helper never runs,
/// and two scans leave the index's bytes and time as they were and agree.
#[test]
fn the_probes_run_no_helper_and_write_no_index() {
    let Some(h) = Home::ready("the_probes_run_no_helper_and_write_no_index") else { return };
    let (main, wt) = project(&h);
    let marker = h.t.path("fsmonitor-ran");
    let helper = h.t.write("fsmonitor.sh", &format!("#!/bin/sh\necho ran >> '{}'\nexit 1\n", marker.display()));
    common::set_mode(&helper, 0o755);
    git::git(&main, &["config", "core.fsmonitor", helper.to_str().unwrap()]);
    // A plain status runs it: the fixture would catch a probe that did.
    let _ = git::try_git(&wt, &["status", "--porcelain"]);
    let runs = |p: &Path| fs::read_to_string(p).map_or(0, |t| t.lines().count());
    let before = runs(&marker);
    assert!(before > 0, "the helper runs on a plain status");
    let index = git::gitdir(&wt).join("index");
    let (bytes, mtime) = (fs::read(&index).unwrap(), fs::metadata(&index).unwrap().modified().unwrap());

    let (first, commands) = h.scan();
    let (second, _) = h.scan();

    assert_eq!(runs(&marker), before, "the helper never ran under a probe");
    assert_eq!(fs::read(&index).unwrap(), bytes, "the index's bytes");
    assert_eq!(fs::metadata(&index).unwrap().modified().unwrap(), mtime, "the index's time");
    let (a, b) = (h.finding(&first, &wt), h.finding(&second, &wt));
    assert_eq!((&a.worktree, a.selected, &a.hint), (&b.worktree, b.selected, &b.hint));
    eligible(a);
    commands.git_calls().iter().for_each(under_contract);
}

#[test]
fn a_locked_worktree_is_kept_without_a_probe() {
    let Some(h) = Home::ready("a_locked_worktree_is_kept_without_a_probe") else { return };
    let (main, wt) = project(&h);
    git::git(&main, &["worktree", "lock", "--reason", "claude session x (pid 1)", wt.to_str().unwrap()]);

    let (r, commands) = h.scan();

    let f = h.finding(&r, &wt);
    kept(f, "locked; git worktree unlock");
    assert_eq!((state(f).locked, state(f).in_use, state(f).probe), (Some(true), None, WorktreeProbe::Unknown));
    assert!(commands.calls().is_empty(), "{:?}", commands.calls());
}

#[test]
fn a_worktree_a_process_works_in_is_in_use() {
    let Some(h) = Home::ready("a_worktree_a_process_works_in_is_in_use") else { return };
    let (_, wt) = project(&h);
    let env = h.env();
    let commands = Arc::new(SharedCommands::system(&env));
    let agent = ProcInfo { pid: 42, name: "claude".to_owned(), cwd: Some(wt.join("src")), ..ProcInfo::default() };
    let opts = ScanOptions { processes: Processes::fixed(FakeProcs::new(vec![agent])), ..h.opts(&commands) };

    let r = h.scan_with(&env, &opts);

    let f = h.finding(&r, &wt);
    kept(f, "in use by claude (pid 42)");
    assert_eq!((state(f).in_use, state(f).idle_days), (Some(true), None));
    assert!(commands.calls().is_empty());
}

#[test]
fn a_worktree_touched_an_hour_ago_is_within_the_window() {
    let Some(h) = Home::ready_after("a_worktree_touched_an_hour_ago_is_within_the_window", Duration::from_secs(3_600))
    else {
        return;
    };
    let (_, wt) = project(&h);

    let (r, commands) = h.scan();

    let f = h.finding(&r, &wt);
    kept(f, "used 0 days ago, within the 2-day window (--worktree-min-age)");
    assert_eq!((state(f).idle_days, state(f).clean, state(f).probe), (Some(0), None, WorktreeProbe::Unknown));
    assert!(commands.calls().is_empty());

    // A window of 0 days makes it eligible.
    let env = h.env();
    let r = h.scan_with(&env, &ScanOptions { worktree_min_age: Duration::ZERO, ..h.opts(&commands) });
    eligible(h.finding(&r, &wt));
}

#[test]
fn a_persons_own_and_a_codex_worktree_run_no_probe() {
    let Some(h) = Home::ready("a_persons_own_and_a_codex_worktree_run_no_probe") else { return };
    let (main, _) = project(&h);
    let own = git::worktree(&main, &h.t.path("own"), &[]);
    let codex = git::worktree(&main, &h.t.path("codex"), &[]);
    fs::write(git::gitdir(&codex).join("codex-thread.json"), "{}").unwrap();
    let env = h.env();
    let commands = Arc::new(SharedCommands::system(&env));
    // Only the person's and Codex's worktrees: the Claude Code one is locked.
    git::git(&main, &["worktree", "lock", main.join(".claude/worktrees/x").to_str().unwrap()]);

    let r = h.scan_with(&env, &h.opts(&commands));

    let f = h.finding(&r, &own);
    assert_eq!((f.class, f.selected), (RiskClass::Review, false));
    assert!(f.hint.starts_with("yours: a linked worktree of"), "{}", f.hint);
    assert_eq!((state(f).in_use, state(f).idle_days, state(f).probe), (None, None, WorktreeProbe::Unknown));
    kept(h.finding(&r, &codex), "made by Codex");
    assert!(commands.calls().is_empty(), "{:?}", commands.calls());
}

#[test]
fn an_initialized_submodule_keeps_a_clean_worktree() {
    let Some(h) = Home::ready("an_initialized_submodule_keeps_a_clean_worktree") else { return };
    let lib = git::repo(&h.t, "lib");
    let (main, _) = project(&h);
    git::git(&main, &["submodule", "add", "-q", lib.to_str().unwrap(), "libs/lib"]);
    git::git(&main, &["commit", "-q", "-m", "submodule"]);
    let wt = git::worktree(&main, &main.join(".claude/worktrees/sub"), &["-b", "worktree-sub"]);
    git::git(&wt, &["submodule", "update", "-q", "--init"]);
    assert_eq!(git::git(&wt, &["status", "--porcelain"]), "", "clean");
    // The same submodule, not initialized, holds nothing back.
    let bare = git::worktree(&main, &main.join(".claude/worktrees/bare"), &["-b", "worktree-bare"]);

    let (r, _) = h.scan();

    let f = h.finding(&r, &wt);
    kept(f, "a submodule is initialized in it");
    assert_eq!((state(f).submodules, state(f).clean, state(f).probe), (Some(true), None, WorktreeProbe::Known));
    let g = h.finding(&r, &bare);
    eligible(g);
    assert_eq!(state(g).submodules, Some(false));
}

#[test]
fn a_change_or_an_untracked_file_makes_it_dirty() {
    let Some(h) = Home::ready("a_change_or_an_untracked_file_makes_it_dirty") else { return };
    let (main, untracked) = project(&h);
    h.write(&untracked.join("notes.txt"), "mine");
    let changed = git::worktree(&main, &main.join(".claude/worktrees/changed"), &["-b", "worktree-changed"]);
    h.write(&changed.join("README"), "edited\n");

    let (r, _) = h.scan();

    for wt in [&untracked, &changed] {
        let f = h.finding(&r, wt);
        kept(f, "uncommitted changes or untracked files");
        assert_eq!((state(f).clean, state(f).own_commits), (Some(false), None), "{}", wt.display());
    }
}

/// A `.venv` with `pyvenv.cfg` and `uv.lock` is an artifact ddc removes on
/// its own, and a `.env` the same as the main checkout's is a copy: the
/// worktree holds nothing of its own.
#[test]
fn ignored_content_is_accounted_for_by_artifacts_and_copies() {
    let Some(h) = Home::ready("ignored_content_is_accounted_for_by_artifacts_and_copies") else { return };
    let (main, wt) = project(&h);
    h.write(&main.join(".env"), "KEY=1\n");
    h.write(&wt.join(".env"), "KEY=1\n");
    h.write(&wt.join(".venv/pyvenv.cfg"), "home = /usr/bin\n");
    h.t.file(&format!("{}/.venv/lib/site.py", h.rel(&wt)), 10_000);

    let (r, _) = h.scan();

    eligible(h.finding(&r, &wt));
    let venv = h.finding(&r, &wt.join(".venv"));
    assert_eq!((venv.rule.as_str(), venv.class), ("python.venv", RiskClass::Rebuildable));
}

/// What a worktree holds that nothing accounts for: a unique `.env` (a), a
/// local folder (b), a venv without a lock file, whose own rule says
/// `review` (c). Each keeps the worktree, and nothing plans them away.
#[test]
fn unique_ignored_content_keeps_the_worktree() {
    let Some(h) = Home::ready("unique_ignored_content_keeps_the_worktree") else { return };
    let (main_a, a) = project_with(&h, "a", true);
    h.write(&main_a.join(".env"), "KEY=1\n");
    h.write(&a.join(".env"), "KEY=2\n");
    let (_, b) = project_with(&h, "b", true);
    h.write(&b.join("state/local-only.txt"), "only here");
    let (_, c) = project_with(&h, "c", false);
    h.write(&c.join(".venv/pyvenv.cfg"), "home = /usr/bin\n");

    let (r, _) = h.scan();

    for (wt, held) in [(&a, a.join(".env")), (&b, b.join("state")), (&c, c.join(".venv"))] {
        let f = h.finding(&r, wt);
        kept(f, "ignored content no artifact accounts for: ");
        let s = state(f);
        assert_eq!((s.clean, s.own_commits), (Some(true), None));
        assert_eq!(s.ignored_content, Some(IgnoredContent { paths: vec![held.clone()], count: 1 }), "{}", wt.display());
        assert!(f.hint.contains(held.file_name().unwrap().to_str().unwrap()), "{}", f.hint);
    }
    let venv = h.finding(&r, &c.join(".venv"));
    assert_eq!((venv.rule.as_str(), venv.class), ("python.venv", RiskClass::Review), "still its own review finding");
    let plan = build_plan(
        &r,
        &Catalog::builtin(h.t.root()),
        &PlanOptions { classes: vec![RiskClass::Rebuildable], ..PlanOptions::default() },
    );
    for file in [a.join(".env"), b.join("state/local-only.txt"), c.join(".venv/pyvenv.cfg")] {
        assert!(plan.items.iter().all(|i| !file.starts_with(&i.finding.path)), "{} planned", file.display());
    }
}

#[test]
fn more_than_twenty_unaccounted_paths_are_counted() {
    let Some(h) = Home::ready("more_than_twenty_unaccounted_paths_are_counted") else { return };
    let main = git::repo(&h.t, "proj");
    h.write(&main.join(".gitignore"), "*.log\n");
    git::git(&main, &["add", ".gitignore"]);
    git::git(&main, &["commit", "-q", "-m", "ignore"]);
    let wt = git::worktree(&main, &main.join(".claude/worktrees/x"), &[]);
    for i in 0..25 {
        h.write(&wt.join(format!("{i:02}.log")), "x");
    }

    let (r, _) = h.scan();

    let f = h.finding(&r, &wt);
    let held = state(f).ignored_content.clone().unwrap();
    assert_eq!((held.paths.len(), held.count), (20, 25));
    assert_eq!(held.paths[0], wt.join("00.log"));
    assert!(f.hint.contains("00.log and 24 more"), "{}", f.hint);
}

#[test]
fn commits_held_by_a_surviving_ref_are_not_its_own() {
    let Some(h) = Home::ready("commits_held_by_a_surviving_ref_are_not_its_own") else { return };
    let (main, merged) = project(&h);
    commit(&h, &merged, "merged.txt");
    git::git(&main, &["merge", "-q", "--ff-only", "worktree-x"]);
    let unique = git::worktree(&main, &main.join(".claude/worktrees/unique"), &["-b", "worktree-unique"]);
    commit(&h, &unique, "unique.txt");
    let detached = git::worktree(&main, &main.join(".claude/worktrees/detached"), &["--detach"]);
    let lone = git::worktree(&main, &main.join(".claude/worktrees/lone"), &["--detach"]);
    commit(&h, &lone, "lone.txt");
    // A commit only another worktree's detached HEAD holds survives the candidate.
    let held = git::worktree(&main, &main.join(".claude/worktrees/held"), &["-b", "worktree-held"]);
    let tip = commit(&h, &held, "held.txt");
    let other = h.t.path("other");
    git::git(&main, &["worktree", "add", "-q", "--detach", other.to_str().unwrap(), &tip]);

    let (r, commands) = h.scan();

    for wt in [&merged, &detached, &held] {
        let f = h.finding(&r, wt);
        eligible(f);
        assert_eq!(state(f).own_commits, Some(0), "{}", wt.display());
    }
    for wt in [&unique, &lone] {
        let f = h.finding(&r, wt);
        kept(f, "1 commit that no branch, tag, remote or other worktree holds");
        assert_eq!(state(f).own_commits, Some(1), "{}", wt.display());
    }
    // Five worktrees reach condition 6; their repository's references are asked once.
    for asked in ["for-each-ref", "worktree"] {
        let calls = commands.git_calls().iter().filter(|c| c.args.iter().any(|a| a == asked)).count();
        assert_eq!(calls, 1, "{asked}");
    }
    assert_eq!(state(h.finding(&r, &detached)).detached, Some(true));

    // The same answers from the main checkout and from another worktree:
    // the probe names commits, not `HEAD`.
    let env = h.env();
    let system = SystemCommands::new(&env);
    let exe = ddc_core::git::locate(&system, &env.tool_dirs(), PROBE_TIMEOUT).expect("a git");
    let probe = Git::new(&system, exe, PROBE_TIMEOUT);
    for (wt, own) in [(&merged, 0), (&detached, 0), (&held, 0), (&unique, 1), (&lone, 1)] {
        let head = probe.head(wt).known().unwrap();
        for from in [&main, &other] {
            let survivors = probe.survivors(from, wt, head.branch.as_deref()).known().unwrap();
            let counted = probe.own_commits(from, &head.commit, &survivors).known().unwrap();
            assert_eq!(counted, own, "{} from {}", wt.display(), from.display());
        }
    }
}

/// Decision 9: a worktree's idleness is its own; its main checkout,
/// committed to a minute ago, does not hold it back.
#[test]
fn a_busy_main_checkout_does_not_hold_back_an_idle_worktree() {
    let Some(h) = Home::ready("a_busy_main_checkout_does_not_hold_back_an_idle_worktree") else { return };
    let (main, wt) = project(&h);
    let a_minute_ago = h.now.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs() as i64 - 60;
    git::git(&main, &["commit", "-q", "--allow-empty", "-m", "busy"]);
    for f in [".git/index", ".git/HEAD", "uv.lock"] {
        h.t.touch(h.rel(&main.join(f)), a_minute_ago);
    }

    let (r, _) = h.scan();

    let f = h.finding(&r, &wt);
    eligible(f);
    assert_eq!(state(f).idle_days, Some(3));
    assert_eq!(f.project.as_ref().unwrap().idle_days, Some(3), "its own activity, not the main checkout's");
}

/// A git that hangs, one that exits 128 and one that prints garbage: each
/// leaves the worktree `review` with `probe: unknown`, within the timeout,
/// and no probe is tried again.
#[test]
fn a_failing_git_fails_closed() {
    let Some(h) = Home::ready("a_failing_git_fails_closed") else { return };
    let (_, wt) = project(&h);
    for (name, body) in [
        ("hangs", "sleep 30\n"),
        ("exits-128", "echo 'fatal: not a git repository' >&2\nexit 128\n"),
        ("garbage", "echo garbage\n"),
    ] {
        let bin = h.t.dir(&format!("bin-{name}"));
        let exe = h.t.write(&format!("bin-{name}/git"), &format!("#!/bin/sh\n{body}"));
        common::set_mode(&exe, 0o755);
        let env = h.env().with_search_path(vec![bin]);
        let commands = Arc::new(SharedCommands::system(&env));
        let opts = ScanOptions { probe_timeout: Duration::from_secs(2), ..h.opts(&commands) };
        let started = Instant::now();

        let r = h.scan_with(&env, &opts);

        assert!(started.elapsed() < Duration::from_secs(20), "{name}: {:?}", started.elapsed());
        let f = h.finding(&r, &wt);
        kept(f, "git could not tell its state");
        assert_eq!(state(f).probe, WorktreeProbe::Unknown, "{name}");
        let calls = commands.git_calls();
        assert_eq!(calls.len(), 1, "{name}: one probe, never retried: {calls:?}");
        assert_eq!(calls[0].exe, exe);
        assert_eq!(calls[0].timeout, Duration::from_secs(2));
    }
}

/// Decision 10: `/usr/bin/git` is the `xcrun` shim and never runs; the
/// developer directory's git does.
#[cfg(target_os = "macos")]
#[test]
fn the_xcrun_shim_never_runs() {
    let Some(h) = Home::ready("the_xcrun_shim_never_runs") else { return };
    let (_, wt) = project(&h);
    let env = h.env().with_search_path(vec![PathBuf::from("/usr/bin")]);

    let shim_only = Arc::new(SharedCommands::answering(&["/usr/bin/git"], &[]));
    let r = h.scan_with(&env, &h.opts(&shim_only));

    let f = h.finding(&r, &wt);
    kept(f, "no git to ask");
    assert_eq!(state(f).probe, WorktreeProbe::Unknown);
    assert!(shim_only.calls().is_empty(), "{:?}", shim_only.calls());

    let developer = Arc::new(SharedCommands::answering(
        &["/usr/bin/git", "/usr/bin/xcode-select", "/Dev/usr/bin/git"],
        &[("/usr/bin/xcode-select -p", true, "/Dev\n")],
    ));
    let r = h.scan_with(&env, &h.opts(&developer));

    assert_eq!(state(h.finding(&r, &wt)).probe, WorktreeProbe::Unknown, "the fake git answers nothing");
    let calls = developer.git_calls();
    assert!(!calls.is_empty());
    assert!(calls.iter().all(|c| c.exe == Path::new("/Dev/usr/bin/git")), "{calls:?}");
}

/// The runner removes the inherited variables it is told to, and only them.
#[cfg(unix)]
#[test]
fn a_cleared_prefix_leaves_the_rest_of_the_environment() {
    use ddc_core::exec::CommandRunner as _;
    let d = tempfile::tempdir().unwrap();
    let env = Env::with_paths(d.path(), &d.path().join("data"), &d.path().join("logs"), SystemTime::now()).unwrap();
    let system = SystemCommands::new(&env);
    let printenv = Path::new("/usr/bin/env");
    let out = system.run_cleared(printenv, &[], "PATH", &[("GIT_OPTIONAL_LOCKS", "0")], PROBE_TIMEOUT).unwrap();
    assert!(out.success);
    assert!(!out.stdout.lines().any(|l| l.starts_with("PATH=")), "{}", out.stdout);
    assert!(out.stdout.lines().any(|l| l == "GIT_OPTIONAL_LOCKS=0"), "{}", out.stdout);
    let all = system.run_cleared(printenv, &[], "", &[], PROBE_TIMEOUT).unwrap();
    assert!(all.stdout.lines().any(|l| l.starts_with("PATH=")), "an empty prefix clears nothing");
}

/// A worktree that holds the conditions is selected like any other
/// `rebuildable` item, and its bytes count in the selected totals once:
/// the artifacts inside it are findings of their own, which its own bytes
/// leave out.
#[test]
fn an_eligible_worktree_counts_in_the_selected_total_once() {
    let Some(h) = Home::ready("an_eligible_worktree_counts_in_the_selected_total_once") else { return };
    let (_, wt) = project(&h);
    h.t.file(&format!("{}/data.bin", h.rel(&wt)), 50_000);
    git::git(&wt, &["add", "data.bin"]);
    git::git(&wt, &["commit", "-q", "-m", "data"]);
    git::git(&h.t.path("proj"), &["merge", "-q", "--ff-only", "worktree-x"]);
    h.write(&wt.join(".venv/pyvenv.cfg"), "home = /usr/bin\n");
    h.t.file(&format!("{}/.venv/lib/site.py", h.rel(&wt)), 30_000);

    let (r, _) = h.scan();

    let f = h.finding(&r, &wt);
    eligible(f);
    let venv = h.finding(&r, &wt.join(".venv"));
    assert!(f.bytes >= 50_000 && f.bytes < 50_000 + venv.bytes, "its own bytes leave the .venv out: {}", f.bytes);
    let selected: u64 = r.findings.iter().filter(|f| f.selected && !f.ignored).map(|f| f.bytes).sum();
    assert_eq!(r.totals.selected_bytes, selected);
    let rebuildable = r.totals.classes.iter().find(|c| c.class == RiskClass::Rebuildable).unwrap();
    // The .venv's project is the worktree, active three days ago: within `--min-age`.
    assert!(!venv.selected);
    assert_eq!((rebuildable.selected_count, rebuildable.selected_bytes), (1, f.bytes), "{rebuildable:?}");
}

/// A tracked file marked skip-worktree or assume-unchanged keeps a local
/// edit `status` does not see, which `git worktree remove` would delete.
#[test]
fn skip_worktree_and_assume_unchanged_files_keep_the_worktree() {
    let Some(h) = Home::ready("skip_worktree_and_assume_unchanged_files_keep_the_worktree") else { return };
    let (main, sparse) = project(&h);
    git::git(&sparse, &["update-index", "--skip-worktree", "README"]);
    h.write(&sparse.join("README"), "a local edit\n");
    let assumed = git::worktree(&main, &main.join(".claude/worktrees/assumed"), &["-b", "worktree-assumed"]);
    git::git(&assumed, &["update-index", "--assume-unchanged", "uv.lock"]);
    h.write(&assumed.join("uv.lock"), "edited\n");
    for wt in [&sparse, &assumed] {
        assert_eq!(git::git(wt, &["status", "--porcelain"]), "", "status sees nothing");
    }

    let (r, _) = h.scan();

    for (wt, file) in [(&sparse, sparse.join("README")), (&assumed, assumed.join("uv.lock"))] {
        let f = h.finding(&r, wt);
        kept(f, "files git does not check (skip-worktree or assume-unchanged): ");
        let s = state(f);
        assert_eq!((s.clean, s.own_commits), (Some(true), None));
        assert_eq!(s.ignored_content, Some(IgnoredContent { paths: vec![file], count: 1 }), "{}", wt.display());
    }
}

/// An ignored file larger than 64 MiB is never compared with the main
/// checkout's, even when both are the same: it counts as its own.
#[test]
fn a_huge_ignored_file_is_never_compared() {
    let Some(h) = Home::ready("a_huge_ignored_file_is_never_compared") else { return };
    let (main, huge) = project(&h);
    let len = ddc_core::rules::worktree::MAX_COMPARED + 1;
    h.t.sparse(&format!("{}/.env", h.rel(&main)), len);
    h.t.sparse(&format!("{}/.env", h.rel(&huge)), len);

    let (r, _) = h.scan();

    let f = h.finding(&r, &huge);
    kept(f, "ignored content no artifact accounts for: .env");
    assert_eq!(state(f).ignored_content.as_ref().unwrap().paths, vec![huge.join(".env")]);
}

/// A worktree rule switched off in `settings.json` runs no probe.
#[test]
fn a_disabled_worktree_rule_runs_no_probe() {
    let Some(h) = Home::ready("a_disabled_worktree_rule_runs_no_probe") else { return };
    let (_, wt) = project(&h);
    let env = h.env();
    let commands = Arc::new(SharedCommands::system(&env));
    let opts = ScanOptions { disabled_rules: vec!["git.worktree".to_owned()], ..h.opts(&commands) };

    let r = h.scan_with(&env, &opts);

    let f = h.finding(&r, &wt);
    assert!(f.ignored);
    assert_eq!((state(f).in_use, state(f).probe), (None, WorktreeProbe::Unknown));
    assert!(commands.calls().is_empty(), "{:?}", commands.calls());
}
