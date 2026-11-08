//! Story "ddc footprint: a scoped project report" (epic DDC-003): one
//! project's checkouts, each in exactly one bucket, through the scan's own
//! walk and classification. Real repositories made with the `git` on
//! `PATH` (`common::git`); the footprint runs its own git through
//! `SharedCommands`, which records every call.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use common::fakes::{FakeProcs, SharedCommands};
use common::{TreeBuilder, git};
use ddc_core::env::Env;
use ddc_core::exec::{ProcInfo, ProcStart};
use ddc_core::footprint::{
    self, Bucket, CheckoutKind, FootprintReport, HarnessReason, HeldReason, NotScannedReason, Refused, Scope,
};
use ddc_core::git::{PROBE_CONFIG, PROBE_TIMEOUT};
use ddc_core::report::{self, Commands, Processes, ScanOptions};
use ddc_core::scan::{DirListing, DirReader, Progress, ReadError, StdReader, Times, WalkOptions};
use std::collections::HashSet;
use std::sync::Mutex;

/// How long the fixtures have been idle when the footprint looks at them:
/// past the worktree window (2 days), within the projects' (14).
const IDLE: Duration = Duration::from_secs(3 * 86_400 + 3_600);

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

    fn opts(&self, commands: &Arc<SharedCommands>, procs: FakeProcs) -> ScanOptions {
        ScanOptions {
            walk: WalkOptions { threads: Some(4), ..Default::default() },
            processes: Processes::fixed(procs),
            commands: Commands::shared(commands.clone()),
            ..Default::default()
        }
    }

    fn footprint_with(&self, root: &Path, procs: FakeProcs) -> (FootprintReport, Arc<SharedCommands>) {
        let env = self.env();
        let commands = Arc::new(SharedCommands::system(&env));
        let scope = Scope::resolve(root, env.home(), &*commands, &env.tool_dirs(), PROBE_TIMEOUT).unwrap();
        let opts = footprint::scan_options(&scope, self.opts(&commands, procs));
        let scoped = footprint::scoped(&env, scope, &opts, &Progress::default(), None).unwrap();
        (footprint::report(&env, scoped, &opts), commands)
    }

    fn footprint(&self, root: &Path) -> FootprintReport {
        self.footprint_with(root, FakeProcs::new(vec![])).0
    }

    fn write(&self, path: &Path, text: &str) {
        self.t.write(path.strip_prefix(self.t.root()).unwrap().to_str().unwrap(), text);
    }
}

/// `proj` with `.gitignore` and `uv.lock` committed.
fn project(h: &Home) -> PathBuf {
    let main = git::repo(&h.t, "proj");
    h.write(&main.join(".gitignore"), ".venv/\n.env\n");
    h.write(&main.join("uv.lock"), "version = 1\n");
    git::git(&main, &["add", ".gitignore", "uv.lock"]);
    git::git(&main, &["commit", "-q", "-m", "ignore"]);
    main
}

fn harness_worktree(main: &Path, name: &str) -> PathBuf {
    git::worktree(main, &main.join(".claude/worktrees").join(name), &["-b", &format!("worktree-{name}")])
}

fn entry<'a>(r: &'a FootprintReport, path: &Path) -> &'a footprint::CheckoutEntry {
    r.checkouts.iter().find(|c| c.path == path).unwrap_or_else(|| {
        let all: Vec<_> = r.checkouts.iter().map(|c| c.path.display().to_string()).collect();
        panic!("no checkout {}: {all:?}", path.display())
    })
}

#[test]
fn four_worktrees_land_in_their_buckets_and_the_totals_follow() {
    let Some(h) = Home::ready("four_worktrees_land_in_their_buckets_and_the_totals_follow") else { return };
    let main = project(&h);
    let a = harness_worktree(&main, "a");
    let b = harness_worktree(&main, "b");
    let c = harness_worktree(&main, "c");
    let d = harness_worktree(&main, "d");
    h.write(&b.join("new.txt"), "work in progress\n");
    h.write(&c.join(".venv/pyvenv.cfg"), "home = /usr/bin\n");
    h.write(&c.join(".venv/lib/site.py"), &"x".repeat(50_000));
    h.write(&d.join(".env"), "TOKEN=only-here\n");
    h.write(&main.join("pkg/__pycache__/m.cpython-312.pyc"), &"p".repeat(20_000));
    // Beside the main checkout: never part of its footprint.
    h.write(&h.t.root().join("big.iso"), &"y".repeat(300_000));
    h.write(&h.t.root().join("other/node_modules/dep/index.js"), &"z".repeat(40_000));

    let (r, commands) = h.footprint_with(&a, FakeProcs::new(vec![]));
    assert_eq!(r.main, main, "a worktree resolves to its main checkout");
    assert_eq!(r.checkouts[0].kind, CheckoutKind::Main);
    assert_eq!(r.checkouts.len(), 5);
    let (ea, eb, ec, ed) = (entry(&r, &a), entry(&r, &b), entry(&r, &c), entry(&r, &d));
    assert_eq!((ea.bucket, ea.reason), (Some(Bucket::Now), None));
    assert_eq!((ec.bucket, ec.reason), (Some(Bucket::Now), None));
    assert_eq!((eb.bucket, eb.reason), (Some(Bucket::HeldBack), Some(HeldReason::Dirty)));
    assert_eq!((ed.bucket, ed.reason), (Some(Bucket::HeldBack), Some(HeldReason::IgnoredContent)));
    let ignored = ed.worktree.as_ref().and_then(|w| w.ignored_content.as_ref()).expect("ignored content");
    assert_eq!(ignored.paths, [d.join(".env")]);
    let venv = ec.findings.iter().find(|f| f.path == c.join(".venv")).expect("the .venv under C");
    assert_eq!(venv.project.as_ref().and_then(|p| p.worktree_of.clone()), Some(main.clone()));
    assert_eq!(r.totals.reclaimable_now.rebuildable, ea.bytes + ec.bytes, "C's .venv counted once, inside C");
    assert_eq!(r.totals.reclaimable_now.bytes, r.totals.reclaimable_now.safe + ea.bytes + ec.bytes);
    assert_eq!(r.totals.held_back, eb.bytes + ed.bytes);
    assert_eq!(r.clean_roots, std::slice::from_ref(&main), "the suggested clean names the main checkout only");
    // The main checkout's safe cache: by class, and selected now apart from rebuildable.
    let cache = r.checkouts[0].findings.iter().find(|f| f.path == main.join("pkg/__pycache__")).expect("the cache");
    assert!(cache.selected);
    assert_eq!(r.totals.reclaimable_now.safe, cache.bytes);
    assert!(r.totals.safe.count >= 1 && r.totals.safe.bytes >= cache.bytes);
    // Each worktree in exactly one place.
    for c in &r.checkouts[1..] {
        assert!(c.bucket.is_some(), "{}", c.path.display());
        assert!(!r.not_scanned.iter().any(|n| n.path == c.path));
    }
    // Nothing beside the main checkout reaches the report.
    let all: Vec<&Path> = r.checkouts.iter().flat_map(|c| c.findings.iter().map(|f| f.path.as_path())).collect();
    assert!(all.iter().all(|p| p.starts_with(&main)), "{all:?}");
    // Every git call runs under the probe contract.
    for call in commands.git_calls() {
        for flag in PROBE_CONFIG {
            assert!(call.args.iter().any(|a| a == flag), "{:?} lacks {flag}", call.args);
        }
    }
    // Run from a subfolder of the main checkout: the same checkouts.
    let again = h.footprint(&main.join(".claude"));
    assert_eq!(
        again.checkouts.iter().map(|c| &c.path).collect::<Vec<_>>(),
        r.checkouts.iter().map(|c| &c.path).collect::<Vec<_>>()
    );
}

#[test]
fn a_live_lock_waits_for_its_session_and_a_dead_one_is_stale() {
    let Some(h) = Home::ready("a_live_lock_waits_for_its_session_and_a_dead_one_is_stale") else { return };
    let main = project(&h);
    let live = harness_worktree(&main, "live");
    let dead = harness_worktree(&main, "dead");
    git::git(&main, &["worktree", "lock", "--reason", "claude agent live (pid 4242 start 1)", live.to_str().unwrap()]);
    git::git(&main, &["worktree", "lock", "--reason", "claude session dead (pid 5151)", dead.to_str().unwrap()]);
    let me = ProcStart { uid: ddc_core::platform::procs::uid(), sec: 1, usec: 0 };
    let (r, _) = h.footprint_with(&main, FakeProcs::new(vec![]).with_start(4242, me));
    let (el, ed) = (entry(&r, &live), entry(&r, &dead));
    assert_eq!(el.bucket, Some(Bucket::AfterSessions), "{:?}", el.reason);
    assert_eq!(el.lock, Some(footprint::Lock { pid: Some(4242), live: Some(true) }));
    assert_eq!(el.worktree.as_ref().and_then(|w| w.locked), Some(true));
    assert_eq!(el.worktree.as_ref().and_then(|w| w.clean), Some(true), "probed although locked");
    assert_eq!((ed.bucket, ed.reason), (Some(Bucket::HeldBack), Some(HeldReason::StaleLock)));
    assert_eq!(ed.lock.and_then(|l| l.pid), Some(5151));
    assert_eq!(r.totals.reclaimable_after_sessions, el.bytes);
}

#[test]
fn a_scan_still_stops_at_the_first_failed_condition() {
    let Some(h) = Home::ready("a_scan_still_stops_at_the_first_failed_condition") else { return };
    let main = project(&h);
    let wt = harness_worktree(&main, "x");
    git::git(&main, &["worktree", "lock", wt.to_str().unwrap()]);
    let env = h.env();
    let commands = Arc::new(SharedCommands::system(&env));
    let opts = ScanOptions { roots: vec![h.t.root().to_path_buf()], ..h.opts(&commands, FakeProcs::new(vec![])) };
    let r = report::scan(&env, &opts, &Progress::default()).unwrap();
    let f = r.findings.iter().find(|f| f.path == wt).expect("the worktree");
    let state = f.worktree.as_ref().unwrap();
    assert_eq!(state.locked, Some(true));
    assert_eq!((state.clean, state.in_use), (None, None), "a scan probes no locked worktree");
}

#[test]
fn the_report_says_why_the_harness_part_is_missing() {
    let Some(h) = Home::ready("the_report_says_why_the_harness_part_is_missing") else { return };
    let main = project(&h);
    assert_eq!(h.footprint(&main).harness_reason, Some(HarnessReason::NoReport));
    fs::write(h.data.path().join("last-scan.json"), r#"{"schema_version": 1}"#).unwrap();
    assert_eq!(h.footprint(&main).harness_reason, Some(HarnessReason::SchemaMismatch));
    fs::write(h.data.path().join("last-scan.json"), "not json").unwrap();
    assert_eq!(h.footprint(&main).harness_reason, Some(HarnessReason::Unreadable));
    // A report of the current schema: its agent findings, and its age.
    let env = h.env();
    let commands = Arc::new(SharedCommands::system(&env));
    let opts = ScanOptions { roots: vec![main.clone()], ..h.opts(&commands, FakeProcs::new(vec![])) };
    h.write(&main.join("pkg/__pycache__/m.cpython-312.pyc"), &"p".repeat(20_000));
    let mut scan = report::scan(&env, &opts, &Progress::default()).unwrap();
    // One agent finding among the others: only it is kept.
    let mut agent = scan.findings.first().cloned().expect("a finding in the project");
    agent.rule = "agent.claude-transcripts".to_owned();
    scan.findings.push(agent);
    fs::write(h.data.path().join("last-scan.json"), serde_json::to_vec(&scan).unwrap()).unwrap();
    let r = h.footprint(&main);
    assert_eq!(r.harness_reason, None);
    let harness = r.harness.expect("the harness part");
    assert_eq!(harness.findings.len(), 1);
    assert_eq!(harness.findings[0].rule, "agent.claude-transcripts");
    assert_eq!(harness.report_age, 0, "written at the footprint's own now");
}

#[test]
#[allow(clippy::disallowed_methods)] // the fixture removes a worktree's folder by hand
fn a_listed_worktree_whose_folder_is_gone_is_not_scanned_and_the_rest_runs() {
    let Some(h) = Home::ready("a_listed_worktree_whose_folder_is_gone_is_not_scanned_and_the_rest_runs") else {
        return;
    };
    let main = project(&h);
    let outside = git::worktree(&main, &h.t.root().join("proj-wt"), &["-b", "outside"]);
    let gone = harness_worktree(&main, "gone");
    fs::remove_dir_all(&gone).unwrap();
    let r = h.footprint(&main);
    assert!(
        r.not_scanned.iter().any(|n| n.path == gone && n.reason == NotScannedReason::Missing),
        "{:?}",
        r.not_scanned
    );
    let e = entry(&r, &outside);
    assert_eq!((e.bucket, e.reason), (Some(Bucket::HeldBack), Some(HeldReason::Yours)), "made by git worktree add");
    assert_eq!(r.clean_roots, std::slice::from_ref(&main), "never the outside worktree's parent");
}

#[test]
#[allow(clippy::disallowed_methods)] // the fixture removes a worktree's gitdir by hand
fn an_orphan_is_held_back_and_a_plain_folder_is_not_a_checkout() {
    let Some(h) = Home::ready("an_orphan_is_held_back_and_a_plain_folder_is_not_a_checkout") else { return };
    let main = project(&h);
    let orphan = harness_worktree(&main, "orphan");
    fs::remove_dir_all(git::gitdir(&orphan)).unwrap();
    h.write(&main.join(".claude/worktrees/plain/file.txt"), "x");
    let r = h.footprint(&main);
    // git no longer lists it: the walk finds it inside the main checkout.
    let o = r.checkouts.iter().find(|c| c.kind == CheckoutKind::Orphan).expect("the orphan is a checkout");
    assert_eq!(o.path, orphan);
    assert_eq!((o.bucket, o.reason), (Some(Bucket::HeldBack), Some(HeldReason::Orphan)));
    assert!(!r.checkouts[0].findings.iter().any(|f| f.path == orphan), "not under main's findings");
    assert!(!r.checkouts.iter().any(|c| c.path == main.join(".claude/worktrees/plain")));
}

#[test]
fn outside_a_repository_or_at_the_home_it_refuses() {
    let Some(h) = Home::ready("outside_a_repository_or_at_the_home_it_refuses") else { return };
    let plain = h.t.dir("plain");
    assert!(!plain.ancestors().any(|a| a.join(".git").exists()), "the temporary folder lies inside a repository");
    let home = h.env().home().to_path_buf();
    assert!(matches!(footprint::resolve_main(&plain, &home), Err(Refused::NotARepository { .. })));
    git::git(&home, &["init", "-q", "-b", "main"]);
    assert!(matches!(footprint::resolve_main(&plain, &home), Err(Refused::HomeIsTheCheckout { .. })));
}

#[test]
fn a_process_in_it_waits_for_its_session_and_a_lock_without_a_table_is_live() {
    let Some(h) = Home::ready("a_process_in_it_waits_for_its_session_and_a_lock_without_a_table_is_live") else {
        return;
    };
    let main = project(&h);
    let wt = harness_worktree(&main, "busy");
    let agent = ProcInfo { pid: 42, name: "claude".to_owned(), cwd: Some(wt.join("src")), ..ProcInfo::default() };
    let (r, _) = h.footprint_with(&main, FakeProcs::new(vec![agent]));
    let e = entry(&r, &wt);
    assert_eq!(e.bucket, Some(Bucket::AfterSessions), "{:?}", e.reason);
    assert_eq!(e.worktree.as_ref().and_then(|w| w.in_use), Some(true));
    assert_eq!(e.worktree.as_ref().and_then(|w| w.clean), Some(true), "probed although in use");
}

#[test]
fn a_skipped_worktree_is_not_scanned_and_an_excluded_one_is_kept() {
    let Some(h) = Home::ready("a_skipped_worktree_is_not_scanned_and_an_excluded_one_is_kept") else { return };
    let main = project(&h);
    let skipped = harness_worktree(&main, "skipped");
    let excluded = harness_worktree(&main, "excluded");
    let env = h.env();
    let commands = Arc::new(SharedCommands::system(&env));
    let scope = Scope::resolve(&main, env.home(), &*commands, &env.tool_dirs(), PROBE_TIMEOUT).unwrap();
    let base = ScanOptions {
        user_skips: vec![skipped.clone()],
        excludes: vec![excluded.clone()],
        ..h.opts(&commands, FakeProcs::new(vec![]))
    };
    let opts = footprint::scan_options(&scope, base);
    let scoped = footprint::scoped(&env, scope, &opts, &Progress::default(), None).unwrap();
    let r = footprint::report(&env, scoped, &opts);
    assert!(
        r.not_scanned.iter().any(|n| n.path == skipped && n.reason == NotScannedReason::Skipped),
        "{:?}",
        r.not_scanned
    );
    let e = entry(&r, &excluded);
    assert_eq!((e.bucket, e.reason), (Some(Bucket::HeldBack), Some(HeldReason::Kept)));
    assert!(e.worktree.is_none() && e.origin.is_none(), "a claim, not a worktree region");
}

#[test]
fn a_bare_repositorys_worktrees_are_walked_and_the_bare_folder_is_not() {
    let Some(h) = Home::ready("a_bare_repositorys_worktrees_are_walked_and_the_bare_folder_is_not") else { return };
    let src = project(&h);
    let bare = h.t.root().join("proj.git");
    git::git(h.t.root(), &["clone", "-q", "--bare", src.to_str().unwrap(), bare.to_str().unwrap()]);
    let wt = git::worktree(&bare, &h.t.root().join("bare-wt"), &["-b", "feature"]);
    let r = h.footprint(&wt);
    assert!(r.bare, "{}", r.main.display());
    assert_eq!(r.main, bare);
    assert!(r.checkouts.iter().all(|c| c.path != bare), "the bare folder is never a checkout");
    assert!(r.checkouts.iter().any(|c| c.path == wt));
    assert!(r.clean_roots.is_empty(), "a bare repository has no checkout to clean");
}

#[test]
fn a_submodule_is_its_own_project_and_an_orphan_with_nothing_above_is_none() {
    let Some(h) = Home::ready("a_submodule_is_its_own_project_and_an_orphan_with_nothing_above_is_none") else {
        return;
    };
    let home = h.env().home().to_path_buf();
    // A submodule's `.git` file: its gitdir has no `commondir`, so the folder
    // is the main checkout of its own repository.
    h.t.write("outer/.git/modules/sub/HEAD", "ref: refs/heads/main\n");
    h.t.write("outer/sub/.git", "gitdir: ../.git/modules/sub\n");
    let sub = h.t.root().join("outer/sub");
    assert_eq!(footprint::resolve_main(&sub, &home).map(|m| m.0), Ok(sub.clone()));
    // An orphan's `.git` file with no repository above it.
    h.t.write("lone/.git", "gitdir: ../gone/.git/worktrees/lone\n");
    let lone = h.t.root().join("lone");
    assert_eq!(footprint::resolve_main(&lone, &home), Err(Refused::NotARepository { root: lone.clone() }));
}

#[test]
fn a_worktree_git_cannot_probe_is_held_back_unknown() {
    let Some(h) = Home::ready("a_worktree_git_cannot_probe_is_held_back_unknown") else { return };
    let main = project(&h);
    let wt = harness_worktree(&main, "x");
    let env = h.env();
    // The listing asks the real git; the walk's probes find none.
    let listing = SharedCommands::system(&env);
    let scope = Scope::resolve(&main, env.home(), &listing, &env.tool_dirs(), PROBE_TIMEOUT).unwrap();
    let no_git = Arc::new(SharedCommands::answering(&[], &[]));
    let opts = footprint::scan_options(&scope, h.opts(&no_git, FakeProcs::new(vec![])));
    let scoped = footprint::scoped(&env, scope, &opts, &Progress::default(), None).unwrap();
    let r = footprint::report(&env, scoped, &opts);
    let e = entry(&r, &wt);
    assert_eq!((e.bucket, e.reason), (Some(Bucket::HeldBack), Some(HeldReason::ProbeUnknown)));
}

/// Records every folder the walk opens.
#[derive(Default)]
struct Counting {
    opened: Mutex<HashSet<PathBuf>>,
}

impl DirReader for Counting {
    fn read(
        &self,
        dir: &Path,
        expect: Option<(u64, u64)>,
        want_private: bool,
        times: Times,
    ) -> Result<DirListing, ReadError> {
        self.opened.lock().unwrap().insert(dir.to_path_buf());
        StdReader.read(dir, expect, want_private, times)
    }
}

#[test]
fn the_walk_opens_only_the_checkouts_and_their_parents_and_changes_no_index() {
    let Some(h) = Home::ready("the_walk_opens_only_the_checkouts_and_their_parents_and_changes_no_index") else {
        return;
    };
    let main = project(&h);
    let a = harness_worktree(&main, "a");
    h.write(&h.t.root().join("other/src/lib.rs"), "fn x() {}");
    h.write(&h.t.root().join("other/node_modules/dep/index.js"), "x");
    let index = git::gitdir(&a).join("index");
    let before = (fs::read(&index).unwrap(), fs::metadata(&index).unwrap().modified().unwrap());

    let env = h.env();
    let commands = Arc::new(SharedCommands::system(&env));
    let scope = Scope::resolve(&a, env.home(), &*commands, &env.tool_dirs(), PROBE_TIMEOUT).unwrap();
    let opts = footprint::scan_options(&scope, h.opts(&commands, FakeProcs::new(vec![])));
    let reader = Counting::default();
    let scoped = footprint::scoped(&env, scope, &opts, &Progress::default(), Some(&reader)).unwrap();
    let r = footprint::report(&env, scoped, &opts);
    assert_eq!(r.checkouts.len(), 2);
    let opened = reader.opened.lock().unwrap().clone();
    for dir in &opened {
        let inside = dir.starts_with(&main);
        let parent = main.ancestors().any(|p| p == dir);
        assert!(inside || parent, "opened {} outside the checkouts and their parents", dir.display());
    }
    assert!(!opened.iter().any(|d| d.starts_with(h.t.root().join("other"))), "a sibling project is never opened");
    let after = (fs::read(&index).unwrap(), fs::metadata(&index).unwrap().modified().unwrap());
    assert!(before == after, "the worktree's index bytes and time are unchanged");
}

#[test]
#[allow(clippy::disallowed_methods)] // DDC_UPDATE_SCHEMA regenerates the committed file
fn committed_footprint_schema_is_current() {
    let schema = serde_json::to_string_pretty(&schemars::schema_for!(FootprintReport)).unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/footprint.schema.json");
    if std::env::var_os("DDC_UPDATE_SCHEMA").is_some() {
        fs::write(&path, &schema).unwrap();
    }
    let committed = fs::read_to_string(&path).unwrap_or_default();
    assert!(committed == schema, "docs/footprint.schema.json is stale: run DDC_UPDATE_SCHEMA=1 cargo test");
}
