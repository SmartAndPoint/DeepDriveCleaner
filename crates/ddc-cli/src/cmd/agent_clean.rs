//! `ddc agent clean [ROOT]`: a coding agent's one cleanup entry point
//! (story "Clean: --agent bounded consent"; ADR "Agent footprint", §7). It
//! removes only the `safe` project artifacts inside one project's checkouts,
//! and only while `settings.json` has `agents_clean_safe: true`; everything
//! else a person may clean is listed for them. It never writes
//! `last-scan.json`: its walk covers the checkouts only.

use std::io::{IsTerminal as _, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context as _;
use ddc_core::env::Env;
use ddc_core::exec::report::{AgentPart, CleanReport};
use ddc_core::exec::{
    self, Consent, ExecOptions, ExecReport, FileJournal, Outcome, PROBE_TIMEOUT, Ports, SystemCommands,
    SystemProcesses, SystemTrash,
};
use ddc_core::footprint::{self, Scope};
use ddc_core::holders::HolderCommand;
use ddc_core::plan::{AgentBound, ListedItems, plan_agent};
use ddc_core::platform::{procs, signals};
use ddc_core::report::recheck;
use ddc_core::report::volumes::SystemStatFs;
use ddc_core::rules::Catalog;
use ddc_core::scan::Progress;
use ddc_core::settings::{self, ScanFlags, Settings};
use ddc_core::store::{self, LockMode};

use crate::cli::AgentCleanArgs;
use crate::exit;
use crate::render::{self, Style};

pub fn run(args: &AgentCleanArgs) -> anyhow::Result<ExitCode> {
    if procs::is_root() {
        return refuse("refusing to run as root; run it as your user");
    }
    // Refused before the data directory, the lock or the journal is touched.
    let list = match &args.items {
        None => None,
        Some(path) => {
            let bytes = match super::read_list(path) {
                Ok(bytes) => bytes,
                Err(e) => return refuse(&format!("cannot read {}: {e}", path.display())),
            };
            match ListedItems::parse(&bytes) {
                Ok(list) => Some(list),
                Err(e) => return refuse(&e.to_string()),
            }
        }
    };
    let env = Env::from_process(args.data_dir.clone())?;
    let root = match &args.root {
        Some(root) => root.clone(),
        None => std::env::current_dir().context("cannot read the current folder")?,
    };
    let commands = SystemCommands::new(&env);
    let scope = Scope::resolve(&root, env.home(), &commands, &env.tool_dirs(), PROBE_TIMEOUT)?;
    let journal_path = FileJournal::path_in(&env);
    let part = |opt_in: bool, settings_error: Option<String>, left_out: Vec<(PathBuf, String)>| AgentPart {
        project: Some(scope.main.clone()),
        agents_clean_safe: opt_in,
        dry_run: args.dry_run,
        settings_error,
        left_out,
    };
    // Unreadable settings: the skips are unknown, so nothing is considered.
    let unreadable = |e: &dyn std::fmt::Display| {
        CleanReport::for_agent(&[], &[], None, journal_path.clone(), part(false, Some(e.to_string()), Vec::new()))
    };
    let mut settings = match settings::load(&env) {
        Ok(settings) => settings,
        Err(e) => return finish(args, &env, &unreadable(&e), None, &[]),
    };
    // A real run holds the exclusive lock from here and reads the settings
    // again under it: an opt-in switched off before that is seen.
    let mut lock = None;
    if settings.agents_clean_safe && !args.dry_run {
        lock = Some(store::try_lock(&env, LockMode::Exclusive, HolderCommand::AgentClean)?);
        settings = match settings::load(&env) {
            Ok(settings) => settings,
            Err(e) => return finish(args, &env, &unreadable(&e), None, &[]),
        };
        if !settings.agents_clean_safe {
            lock = None;
        }
    }
    let opt_in = settings.agents_clean_safe;
    // Settings' skips, excludes and disabled rules apply as in a scan.
    let resolved = settings.resolve(&env, &ScanFlags { paths: vec![scope.main.clone()], ..ScanFlags::default() })?;
    super::note_missing(&resolved.missing);
    ddc_core::report::require_own_roots(&env, &resolved.options)?;
    let opts = footprint::scan_options(&scope, resolved.options);
    let walked = recheck::scoped(&env, &opts, &scope.reach(), &Progress::default(), &SystemStatFs, None)?;
    let bound = AgentBound::new(&walked, &scope.main, scope.bare, &scope.checkouts, env.home());
    let catalog = Catalog::builtin(&env);
    let plan = plan_agent(&walked, &catalog, &bound, list.as_ref().map(|l| l.items.as_slice()), opt_in);

    let run = if lock.is_some() && !plan.plan.items.is_empty() {
        let skips: Vec<PathBuf> = opts.skip.iter().chain(&opts.user_skips).cloned().collect();
        let mut journal = FileJournal::open(&env).context("cannot open the action journal")?;
        let cancel = signals::install().context("cannot install the signal handler")?;
        let result = exec::execute(
            &env,
            &catalog,
            &plan.plan,
            &Consent::Agent { bound: bound.clone() },
            Ports {
                commands: &commands,
                trash: &SystemTrash::default(),
                procs: &SystemProcesses,
                journal: &mut journal,
            },
            &ExecOptions {
                cancel,
                skips: &skips,
                worktree_min_age: opts.worktree_min_age,
                probe_timeout: opts.probe_timeout,
                ..ExecOptions::default()
            },
        )?;
        Some(result)
    } else {
        None
    };
    drop(lock);
    let clean = CleanReport::for_agent(
        &plan.items,
        &plan.for_person,
        run.as_ref(),
        journal_path,
        part(opt_in, None, bound.left_out.clone()),
    );
    let folders = person_folders(&env, &settings, &bound, &clean);
    finish(args, &env, &clean, run.as_ref(), &folders)
}

/// The folders `ddc clean --items` must walk to reach the person's items:
/// none when the scan roots of `settings.json` hold them all, else the
/// parents of the bound's checkouts (never of one left out of it, which
/// may be another account's or above the home folder).
fn person_folders(env: &Env, settings: &Settings, bound: &AgentBound, clean: &CleanReport) -> Vec<PathBuf> {
    let Some(list) = &clean.for_person else { return Vec::new() };
    let mut roots = settings.resolve(env, &ScanFlags::default()).map(|r| r.options.roots).unwrap_or_default();
    // No roots in the settings: the home folder.
    if roots.is_empty() {
        roots.push(env.home().to_path_buf());
    }
    let reached = list.items.iter().all(|i| roots.iter().any(|r| i.path.starts_with(r)));
    if reached {
        return Vec::new();
    }
    let mut folders: Vec<PathBuf> = Vec::new();
    for parent in bound.checkouts.iter().filter_map(|c| c.parent()) {
        if !folders.iter().any(|f| f == parent) {
            folders.push(parent.to_path_buf());
        }
    }
    folders
}

fn finish(
    args: &AgentCleanArgs,
    env: &Env,
    clean: &CleanReport,
    run: Option<&ExecReport>,
    folders: &[PathBuf],
) -> anyhow::Result<ExitCode> {
    let mut out = std::io::stdout().lock();
    let written = if args.json {
        serde_json::to_writer_pretty(&mut out, clean).map_err(std::io::Error::from).and_then(|()| writeln!(out))
    } else {
        let style = Style { color: render::use_color(std::io::stdout().is_terminal(), Env::no_color()), all: false };
        render::agent_clean(&mut out, clean, folders, env.home(), &style)
    };
    match written {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        other => other?,
    }
    out.flush().ok();
    // A signal stopped the run: end by it, with the report already on stdout.
    if let Some(sig) = signals::received() {
        ddc_core::holders::end_by_signal(sig);
    }
    // As `clean --items`: with --json the report carries failures.
    let failed = run.is_some_and(|r| {
        r.items.iter().any(|i| matches!(i.outcome, Outcome::Failed { .. } | Outcome::RemovedNotRecycled))
    });
    Ok(if !args.json && failed { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

fn refuse(why: &str) -> anyhow::Result<ExitCode> {
    eprintln!("ddc agent clean: {why}");
    Ok(ExitCode::from(exit::REFUSED))
}
