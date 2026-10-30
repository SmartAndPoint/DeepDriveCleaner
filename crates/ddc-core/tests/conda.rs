//! Story "Whole disk: Conda environments and its package cache, with conda"
//! (safety ADR, amendment of 2026-10-29), the home half: the two rules, the
//! install that claims each finding, and what its conda is given. A temporary
//! home and system root; every conda is a `FakeCommands` answer keyed by its
//! absolute `bin/conda`, never a real conda.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use common::TreeBuilder;
use common::fakes::{FakeCommands, FakeProcs, FakeTrash};
use ddc_core::class::RiskClass;
use ddc_core::conda;
use ddc_core::env::Env;
use ddc_core::exec::preflight::{self, Prediction, PreflightLine, PreflightPorts};
use ddc_core::exec::report::CleanReport;
use ddc_core::exec::{self, Consent, ExecOptions, MemoryJournal, Outcome, Ports, ProcessTable, SkipReason};
use ddc_core::model::ScanReport;
use ddc_core::plan::{ActionKind, ListedItem, plan_listed};
use ddc_core::platform::procs::ProcInfo;
use ddc_core::report::volumes::SystemStatFs;
use ddc_core::report::{self, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::rules::listing::group_of;
use ddc_core::scan::{Progress, StdReader, WalkOptions};

const HOUR: Duration = Duration::from_secs(3_600);

/// What every install is asked, once per clean.
const SHOW: &str = "config --show root_prefix envs_dirs pkgs_dirs allow_softlinks always_softlink --json";
/// What the claiming install is asked before a cache is cleaned.
const SHOW_PKGS: &str = "config --show pkgs_dirs --json";
const CLEAN: &str = "clean --index-cache --packages --tarballs --logfiles --yes";

/// A home with a miniconda install, and a system root whose `/opt` is empty
/// until a test puts an install there.
struct Fx {
    home: TreeBuilder,
    sys: TreeBuilder,
    data: tempfile::TempDir,
    now: SystemTime,
}

/// conda's computed settings, as `config --show … --json` prints them.
fn settings(root: &Path, envs: &[&Path], pkgs: &[&Path], allow: bool, always: bool) -> String {
    serde_json::json!({
        "root_prefix": root,
        "envs_dirs": envs,
        "pkgs_dirs": pkgs,
        "allow_softlinks": allow,
        "always_softlink": always,
    })
    .to_string()
}

fn pkgs_answer(pkgs: &[&Path]) -> String {
    serde_json::json!({ "pkgs_dirs": pkgs }).to_string()
}

impl Fx {
    fn new() -> Self {
        let fx = Self {
            home: TreeBuilder::new(),
            sys: TreeBuilder::new(),
            data: tempfile::tempdir().unwrap(),
            now: SystemTime::now() + HOUR,
        };
        fx.sys.dir("opt");
        // The base environment, an environment of it, and its two package folders.
        fx.home.write("miniconda3/bin/conda", "#!/bin/sh\nexit 1\n");
        fx.home.write("miniconda3/conda-meta/history", "");
        fx.home.write("miniconda3/envs/old/conda-meta/history", "");
        fx.home.file("miniconda3/envs/old/lib/libpython.dylib", 20_000);
        fx.home.file("miniconda3/pkgs/numpy-1.26/lib/numpy.so", 20_000);
        fx.home.file(".conda/pkgs/scipy-1.11/lib/scipy.so", 20_000);
        fx
    }

    fn env(&self) -> Env {
        Env::with_paths(self.home.root(), self.data.path(), &self.data.path().join("logs"), self.now)
            .unwrap()
            .with_system_root(self.sys.root())
    }

    fn catalog(&self) -> Catalog {
        Catalog::builtin(self.home.root())
    }

    fn prefix(&self) -> PathBuf {
        self.home.path("miniconda3")
    }

    fn exe(&self) -> PathBuf {
        conda::exe(&self.prefix())
    }

    fn opt_prefix(&self) -> PathBuf {
        self.sys.write("opt/miniconda3/conda-meta/history", "");
        self.sys.path("opt/miniconda3")
    }

    fn environment(&self) -> PathBuf {
        self.home.path("miniconda3/envs/old")
    }

    fn cache(&self) -> PathBuf {
        self.home.path(".conda/pkgs")
    }

    /// The home install's answer with its defaults: its `envs` and the two
    /// package folders, no symlinks.
    fn home_settings(&self) -> String {
        settings(
            &self.prefix(),
            &[&self.home.path("miniconda3/envs"), &self.home.path(".conda/envs")],
            &[&self.home.path("miniconda3/pkgs"), &self.cache()],
            false,
            false,
        )
    }

    /// The home install, answering its settings and, for the scope check,
    /// exactly `~/.conda/pkgs`.
    fn commands(&self) -> FakeCommands {
        FakeCommands::default()
            .at_path(&self.exe(), SHOW, true, &self.home_settings())
            .at_path(&self.exe(), SHOW_PKGS, true, &pkgs_answer(&[&self.cache()]))
            .at_path(&self.exe(), CLEAN, true, "")
    }

    fn scan(&self) -> ScanReport {
        let opts =
            ScanOptions { walk: WalkOptions { threads: Some(4), ..WalkOptions::default() }, ..Default::default() };
        report::scan_with_entries(&self.env(), &opts, &Progress::default(), &SystemStatFs).unwrap().0
    }

    fn listed(&self, path: &Path) -> ListedItem {
        let scanned = self.scan();
        let f = scanned.findings.iter().find(|f| f.path == path).unwrap_or_else(|| panic!("no finding at {path:?}"));
        ListedItem {
            path: f.path.clone(),
            dev: f.dev,
            ino: f.ino,
            rule: f.rule.clone(),
            class: f.class,
            action: ActionKind::Native,
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

    fn clean_one(&self, path: &Path, commands: &FakeCommands) -> Outcome {
        self.clean(&[self.listed(path)], commands, &FakeProcs::new(vec![])).remove(0)
    }
}

fn not_ready(detail: &str) -> Outcome {
    Outcome::Skipped(SkipReason::ToolNotReady { program: "conda".into(), detail: detail.into() })
}

fn count(calls: &[String], call: &str) -> usize {
    calls.iter().filter(|c| *c == call).count()
}

/// The environment variables `exe` was run with for `call`.
fn env_of(commands: &FakeCommands, exe: &Path, call: &str) -> Vec<String> {
    let full = format!("{} {call}", exe.display());
    let at = commands.path_calls.borrow().iter().position(|c| *c == full).unwrap_or_else(|| panic!("{full} never ran"));
    commands.envs.borrow()[at].clone()
}

/// conda removing the environment: the fake moves it out of the home.
fn removes(commands: FakeCommands, env: &Path, out: &Path) -> FakeCommands {
    let (env, out) = (env.to_path_buf(), out.to_path_buf());
    let call = format!("conda env remove -p {} --yes", env.display());
    FakeCommands {
        on_call: Some(Box::new(move |c| {
            if c == call {
                fs::rename(&env, &out).unwrap();
            }
        })),
        ..commands
    }
}

#[test]
fn an_environment_and_a_package_folder_are_found_and_the_base_is_not() {
    if common::unsupported("an_environment_and_a_package_folder_are_found_and_the_base_is_not") {
        return;
    }
    let fx = Fx::new();
    let scanned = fx.scan();
    let at = |p: &Path| scanned.findings.iter().find(|f| f.path == p);

    let env = at(&fx.environment()).expect("the environment is a finding");
    assert_eq!(env.rule, "python.conda-env");
    assert_eq!(env.class, RiskClass::Review);
    assert_eq!(group_of(&env.rule), Some("Python environments"));

    let cache = at(&fx.cache()).expect("~/.conda/pkgs is a finding");
    assert_eq!(cache.rule, "cache.conda");
    assert_eq!(group_of(&cache.rule), Some("Package downloads"));
    assert_eq!(at(&fx.home.path("miniconda3/pkgs")).map(|f| f.rule.as_str()), Some("cache.conda"));

    assert!(at(&fx.prefix()).is_none(), "the base environment is never a finding");
}

#[test]
fn each_install_is_asked_once_per_clean_and_once_per_cache_and_the_dry_run_asks_none() {
    if common::unsupported("each_install_is_asked_once_per_clean_and_once_per_cache_and_the_dry_run_asks_none") {
        return;
    }
    let fx = Fx::new();
    let env = fx.environment();
    let commands = removes(fx.commands(), &env, &fx.data.path().join("removed"));
    let items = [fx.listed(&env), fx.listed(&fx.cache())];

    let predicted = fx.dry_run(&items, &commands, &FakeProcs::new(vec![]));
    assert_eq!(predicted, [Prediction::WouldRun, Prediction::WouldRun]);
    assert!(commands.path_calls.borrow().is_empty(), "the dry run runs no conda");

    let outcomes = fx.clean(&items, &commands, &FakeProcs::new(vec![]));
    let calls = commands.calls_of(&fx.exe());
    assert_eq!(count(&calls, SHOW), 1, "{calls:?}");
    assert_eq!(count(&calls, SHOW_PKGS), 1, "{calls:?}");
    assert!(matches!(&outcomes[0], Outcome::Native { .. }), "{outcomes:?}");
    assert!(matches!(&outcomes[1], Outcome::Native { .. }), "{outcomes:?}");
    assert!(!env.exists());
}

#[test]
fn conda_gets_the_one_folder_and_only_the_four_clean_flags_and_the_journal_names_its_bin_conda() {
    if common::unsupported(
        "conda_gets_the_one_folder_and_only_the_four_clean_flags_and_the_journal_names_its_bin_conda",
    ) {
        return;
    }
    let fx = Fx::new();
    let env = fx.environment();
    let commands = removes(fx.commands(), &env, &fx.data.path().join("removed"));
    let outcomes = fx.clean(&[fx.listed(&env), fx.listed(&fx.cache())], &commands, &FakeProcs::new(vec![]));

    let calls = commands.calls_of(&fx.exe());
    let remove = format!("env remove -p {} --yes", env.display());
    assert_eq!(count(&calls, &remove), 1, "{calls:?}");
    assert_eq!(count(&calls, CLEAN), 1, "{calls:?}");
    for call in &calls {
        for never in ["--all", "--tempfiles", "--force-pkgs-dirs", "--name", " -n "] {
            assert!(!call.contains(never), "{call}");
        }
    }
    let scoped = format!("CONDA_PKGS_DIRS={}", fx.cache().display());
    assert_eq!(env_of(&commands, &fx.exe(), CLEAN), std::slice::from_ref(&scoped));
    assert_eq!(env_of(&commands, &fx.exe(), SHOW_PKGS), [scoped]);
    assert!(env_of(&commands, &fx.exe(), &remove).is_empty());
    assert!(commands.exes.borrow().iter().all(|e| *e == fx.exe()), "no conda from the tool path");

    for outcome in &outcomes {
        let Outcome::Native { commands } = outcome else { panic!("{outcome:?}") };
        assert_eq!(commands[0].program, fx.exe().display().to_string());
    }
}

#[test]
fn a_second_existing_package_folder_skips_the_cache_and_one_that_does_not_exist_is_dropped() {
    if common::unsupported("a_second_existing_package_folder_skips_the_cache_and_one_that_does_not_exist_is_dropped") {
        return;
    }
    let fx = Fx::new();
    let cache = fx.cache();
    let other = fx.home.path("miniconda3/pkgs");
    let commands = fx.commands().at_path(&fx.exe(), SHOW_PKGS, true, &pkgs_answer(&[&cache, &other]));
    let item = fx.listed(&cache);
    assert_eq!(fx.dry_run(std::slice::from_ref(&item), &commands, &FakeProcs::new(vec![])), [Prediction::WouldRun]);
    assert_eq!(fx.clean(&[item], &commands, &FakeProcs::new(vec![])), [not_ready(conda::NOT_SCOPED)]);
    assert_eq!(count(&commands.calls_of(&fx.exe()), CLEAN), 0);
    assert!(cache.join("scipy-1.11/lib/scipy.so").exists());

    let gone = fx.home.path("nowhere/pkgs");
    let commands = fx.commands().at_path(&fx.exe(), SHOW_PKGS, true, &pkgs_answer(&[&cache, &gone, &cache]));
    assert!(matches!(fx.clean_one(&cache, &commands), Outcome::Native { .. }));
    assert_eq!(count(&commands.calls_of(&fx.exe()), CLEAN), 1);
}

#[test]
fn an_environment_no_install_lists_goes_to_the_trash() {
    if common::unsupported("an_environment_no_install_lists_goes_to_the_trash") {
        return;
    }
    let fx = Fx::new();
    let env = fx.environment();
    let item = fx.listed(&env);
    // No conda at all: the dry run and the clean agree.
    let none = FakeCommands::default();
    assert_eq!(fx.dry_run(std::slice::from_ref(&item), &none, &FakeProcs::new(vec![])), [Prediction::WouldTrash]);
    assert_eq!(fx.clean_one(&env, &none), Outcome::Trashed);
    assert!(!env.exists());

    // An install whose envs_dirs do not hold it.
    let fx = Fx::new();
    let elsewhere = settings(&fx.prefix(), &[&fx.home.path(".conda/envs")], &[&fx.cache()], false, false);
    let commands = FakeCommands::default().at_path(&fx.exe(), SHOW, true, &elsewhere);
    assert_eq!(fx.clean_one(&fx.environment(), &commands), Outcome::Trashed);
    assert_eq!(commands.calls_of(&fx.exe()), [SHOW]);
}

#[test]
fn a_package_folder_no_install_lists_is_skipped_and_stays() {
    if common::unsupported("a_package_folder_no_install_lists_is_skipped_and_stays") {
        return;
    }
    let fx = Fx::new();
    let cache = fx.cache();
    let item = fx.listed(&cache);
    let none = FakeCommands::default();
    let reason = SkipReason::ToolNotReady { program: "conda".into(), detail: conda::NOT_LISTED.into() };
    assert_eq!(
        fx.dry_run(std::slice::from_ref(&item), &none, &FakeProcs::new(vec![])),
        [Prediction::WouldSkip(reason)]
    );
    assert_eq!(fx.clean_one(&cache, &none), not_ready(conda::NOT_LISTED));

    let only_miniconda =
        settings(&fx.prefix(), &[&fx.home.path("miniconda3/envs")], &[&fx.home.path("miniconda3/pkgs")], false, false);
    let commands = FakeCommands::default().at_path(&fx.exe(), SHOW, true, &only_miniconda);
    assert_eq!(fx.clean_one(&cache, &commands), not_ready(conda::NOT_LISTED));
    assert_eq!(commands.calls_of(&fx.exe()), [SHOW]);
    assert!(cache.join("scipy-1.11/lib/scipy.so").exists());
}

#[test]
fn a_path_with_a_comma_is_never_given_to_conda() {
    if common::unsupported("a_path_with_a_comma_is_never_given_to_conda") {
        return;
    }
    let fx = Fx::new();
    fx.home.write("miniconda3/envs/a,b/conda-meta/history", "");
    fx.home.file("miniconda3/envs/a,b/lib/x.dylib", 1_000);
    let env = fx.home.path("miniconda3/envs/a,b");
    let commands = fx.commands();
    assert_eq!(fx.clean_one(&env, &commands), Outcome::Trashed);
    assert!(commands.calls_of(&fx.exe()).iter().all(|c| !c.starts_with("env remove")));
}

#[test]
fn a_probe_that_fails_or_prints_what_ddc_cannot_read_claims_nothing() {
    if common::unsupported("a_probe_that_fails_or_prints_what_ddc_cannot_read_claims_nothing") {
        return;
    }
    for (success, stdout) in [(false, None), (true, Some("not json")), (true, Some("{\"root_prefix\": 1}"))] {
        let fx = Fx::new();
        let stdout = stdout.map_or_else(|| fx.home_settings(), str::to_owned);
        let commands = FakeCommands::default().at_path(&fx.exe(), SHOW, success, &stdout);
        assert_eq!(fx.clean_one(&fx.cache(), &commands), not_ready(conda::NOT_LISTED), "{stdout}");
        assert_eq!(fx.clean_one(&fx.environment(), &commands), Outcome::Trashed, "{stdout}");
        assert!(commands.calls_of(&fx.exe()).iter().all(|c| c == SHOW));
    }
}

#[test]
fn an_envs_dirs_entry_that_does_not_exist_is_skipped() {
    if common::unsupported("an_envs_dirs_entry_that_does_not_exist_is_skipped") {
        return;
    }
    let fx = Fx::new();
    let answer = settings(
        &fx.prefix(),
        &[&fx.home.path("gone/envs"), &fx.home.path("miniconda3/envs")],
        &[&fx.cache()],
        false,
        false,
    );
    let env = fx.environment();
    let commands =
        removes(FakeCommands::default().at_path(&fx.exe(), SHOW, true, &answer), &env, &fx.data.path().join("r"));
    assert!(matches!(fx.clean_one(&env, &commands), Outcome::Native { .. }));
}

#[test]
fn an_install_s_base_is_refused_whichever_install_lists_its_parent() {
    if common::unsupported("an_install_s_base_is_refused_whichever_install_lists_its_parent") {
        return;
    }
    let fx = Fx::new();
    let env = fx.environment();
    let opt = fx.opt_prefix();
    // An /opt install whose base is the home install's environment.
    let commands = fx.commands().at_path(
        &conda::exe(&opt),
        SHOW,
        true,
        &settings(&env, &[&opt.join("envs")], &[&opt.join("pkgs")], false, false),
    );
    assert_eq!(fx.clean_one(&env, &commands), Outcome::Skipped(SkipReason::Protected));
    assert!(env.exists());
    assert!(commands.calls_of(&fx.exe()).iter().all(|c| !c.starts_with("env remove")));
}

#[test]
fn an_environment_a_process_runs_in_is_in_use() {
    if common::unsupported("an_environment_a_process_runs_in_is_in_use") {
        return;
    }
    let fx = Fx::new();
    let env = fx.environment();
    let python = ProcInfo { pid: 812, name: "python".into(), env_prefixes: vec![env.clone()], ..ProcInfo::default() };
    let procs = FakeProcs::new(vec![python]);
    let commands = fx.commands();
    let item = fx.listed(&env);
    let reason = SkipReason::InUse { pid: 812, name: "python".into() };
    assert_eq!(fx.dry_run(std::slice::from_ref(&item), &commands, &procs), [Prediction::WouldSkip(reason.clone())]);
    assert_eq!(fx.clean(&[item], &commands, &procs), [Outcome::Skipped(reason)]);
    assert!(env.exists());
}

#[test]
fn conda_exiting_0_with_the_environment_still_there_is_a_failure() {
    if common::unsupported("conda_exiting_0_with_the_environment_still_there_is_a_failure") {
        return;
    }
    let fx = Fx::new();
    let env = fx.environment();
    let remove = format!("env remove -p {} --yes", env.display());
    let commands = fx.commands().at_path(&fx.exe(), &remove, true, "");
    let Outcome::Failed { error } = fx.clean_one(&env, &commands) else { panic!("not a failure") };
    assert!(error.contains("still there"), "{error}");
    assert!(env.exists());
}

#[test]
fn a_cache_is_skipped_when_any_install_that_lists_it_links_by_symlink() {
    if common::unsupported("a_cache_is_skipped_when_any_install_that_lists_it_links_by_symlink") {
        return;
    }
    for (allow, always) in [(false, true), (true, false)] {
        let fx = Fx::new();
        let answer = settings(&fx.prefix(), &[&fx.home.path("miniconda3/envs")], &[&fx.cache()], allow, always);
        let commands = fx.commands().at_path(&fx.exe(), SHOW, true, &answer);
        assert_eq!(fx.clean_one(&fx.cache(), &commands), not_ready(conda::SOFTLINKS));
        assert_eq!(count(&commands.calls_of(&fx.exe()), CLEAN), 0);
    }

    // The home install says no, an /opt install that also lists it says yes.
    let fx = Fx::new();
    let opt = fx.opt_prefix();
    let commands = fx.commands().at_path(
        &conda::exe(&opt),
        SHOW,
        true,
        &settings(&opt, &[&opt.join("envs")], &[&opt.join("pkgs"), &fx.cache()], false, true),
    );
    assert_eq!(fx.clean_one(&fx.cache(), &commands), not_ready(conda::SOFTLINKS));
    assert_eq!(count(&commands.calls_of(&fx.exe()), CLEAN), 0);
    assert_eq!(count(&commands.calls_of(&conda::exe(&opt)), CLEAN), 0);
}

#[test]
fn the_home_install_cleans_a_folder_an_opt_install_also_lists() {
    if common::unsupported("the_home_install_cleans_a_folder_an_opt_install_also_lists") {
        return;
    }
    let fx = Fx::new();
    let opt = fx.opt_prefix();
    let opt_exe = conda::exe(&opt);
    let commands = fx
        .commands()
        .at_path(
            &opt_exe,
            SHOW,
            true,
            &settings(&opt, &[&opt.join("envs")], &[&opt.join("pkgs"), &fx.cache()], false, false),
        )
        .at_path(&opt_exe, SHOW_PKGS, true, &pkgs_answer(&[&fx.cache()]))
        .at_path(&opt_exe, CLEAN, true, "");
    let Outcome::Native { commands: ran } = fx.clean_one(&fx.cache(), &commands) else { panic!("conda did not run") };
    assert_eq!(ran[0].program, fx.exe().display().to_string());
    assert_eq!(count(&commands.calls_of(&fx.exe()), CLEAN), 1);
    assert_eq!(commands.calls_of(&opt_exe), [SHOW], "the /opt install is asked, never run");
}
