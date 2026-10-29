//! `ddc disk free`: an area freed by its owning tool, a simulator runtime by
//! `simctl runtime delete`, Homebrew by `brew cleanup` (story "Whole disk:
//! free space with the owning tool"; safety ADR, amendment of 2026-10-28;
//! menu bar ADR §6 item 11). Only from a confirmation: the app's list, or a
//! prompt at a terminal. There is no `--yes`.

use std::io::{IsTerminal as _, Write as _};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use anyhow::Context as _;
use ddc_core::disk::Sources;
use ddc_core::env::Env;
use ddc_core::exec::{FileJournal, SystemCommands, SystemProcesses};
use ddc_core::free::{self, ActionList, ActionResult, FREE_SCHEMA_VERSION, FreeContext, FreeReport};
use ddc_core::settings::ScanFlags;
use ddc_core::store::{self, LockMode};
use ddc_core::sys::{procs, signals};

use crate::cli::FreeArgs;
use crate::render::{self, Style};
use crate::{exit, prompt};

pub fn run(args: &FreeArgs) -> anyhow::Result<ExitCode> {
    if procs::is_root() {
        return refuse("refusing to run as root; run it as your user");
    }
    // Refused before the lock, the data directory or the journal is touched.
    let listed = match &args.actions {
        Some(path) => {
            let bytes = match super::read_list(path) {
                Ok(bytes) => bytes,
                Err(e) => return refuse(&format!("cannot read {}: {e}", path.display())),
            };
            match ActionList::parse(&bytes) {
                Ok(list) => Some(list),
                Err(e) => return refuse(&e.to_string()),
            }
        }
        None if !std::io::stdin().is_terminal() => {
            return refuse("confirmation needs a terminal; the app passes its confirmed list with --actions");
        }
        None => None,
    };
    let env = Env::from_process(args.data_dir.clone())?;
    let resolved = super::resolve(&env, ScanFlags::default())?;
    super::note_missing(&resolved.missing);
    let sources = args.system_root.as_deref().map_or_else(Sources::macos, Sources::under);
    let commands = SystemCommands::new(&env);
    let skips = resolved.options.user_skips.clone();
    let style = Style { color: render::use_color(std::io::stdout().is_terminal(), Env::no_color()), all: false };

    let list = match listed {
        Some(list) => list,
        // Asked before the exclusive lock: a declined answer leaves no trace.
        // The handler is not installed yet, so Ctrl-C at a question ends the
        // process at once, before anything runs.
        None => {
            let never = AtomicBool::new(false);
            match ask(
                &FreeContext::new(&commands, &sources, env.home(), &skips, &SystemProcesses, &never),
                &env,
                args,
                &style,
            )? {
                Some(list) => list,
                None => return Ok(ExitCode::SUCCESS),
            }
        }
    };
    let cancel = signals::install().context("cannot install the signal handler")?;
    let ctx = FreeContext::new(&commands, &sources, env.home(), &skips, &SystemProcesses, cancel);
    if args.dry_run {
        let _lock = store::try_lock(&env, LockMode::Shared)?;
        let report = FreeReport::new(true, free::dry_run(&ctx, &list));
        print(args, &report, &env, &style)?;
        end_by_signal();
        return Ok(ExitCode::SUCCESS);
    }

    let _lock = store::try_lock(&env, LockMode::Exclusive)?;
    let mut journal = FileJournal::open(&env).context("cannot open the action journal")?;
    let actions = free::run(&ctx, &list, &mut journal).context("cannot write the action journal")?;
    let mut report = FreeReport::new(false, actions);
    report.journal = Some(journal.path.clone());
    // The saved reports follow what was done, after a stop too.
    report.last_scan = free::edit_last_scan(&env, &resolved.options, &report.actions);
    report.last_disk = free::edit_last_disk(&env, &report.actions);
    print(args, &report, &env, &style)?;
    end_by_signal();
    let failed = report.actions.iter().any(|a| matches!(a.result, ActionResult::Failed { .. }));
    Ok(if !args.json && failed { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

/// A signal stopped the run (or arrived after it): end by it, with the
/// report already on stdout.
fn end_by_signal() {
    if let Some(sig) = signals::received() {
        signals::reraise(sig);
    }
}

/// At a terminal without `--actions`: the actions available now with their
/// dry run, under the shared lock, then a question for each, default No.
/// `None`: nothing to run.
fn ask(ctx: &FreeContext<'_>, env: &Env, args: &FreeArgs, style: &Style) -> anyhow::Result<Option<ActionList>> {
    let available = ActionList { schema_version: FREE_SCHEMA_VERSION, actions: free::available(ctx) };
    let mut out = std::io::stdout().lock();
    if available.actions.is_empty() {
        writeln!(
            out,
            "Nothing to free: no simulator runtime can be deleted, /opt/homebrew/bin/brew is not here, and no conda in /opt."
        )?;
        return Ok(None);
    }
    let previews = {
        let _lock = store::try_lock(env, LockMode::Shared)?;
        free::dry_run(ctx, &available)
    };
    render::free(&mut out, &FreeReport::new(true, previews.clone()), env.home(), style)?;
    if args.dry_run {
        return Ok(None);
    }
    writeln!(out)?;
    let mut chosen = Vec::new();
    for (listed, preview) in available.actions.into_iter().zip(&previews) {
        if preview.result != ActionResult::WouldRun {
            continue;
        }
        let what = match listed.action {
            ddc_core::disk::ActionId::SimctlRuntimeDelete => format!(
                "Delete {}? Xcode downloads its SDK's runtime again at the next build for it.",
                render::action_title(listed.action, listed.identifier.as_deref())
            ),
            ddc_core::disk::ActionId::BrewCleanup => {
                "Run brew cleanup? It removes old, unlinked versions in the Cellar and old downloads.".to_owned()
            }
            ddc_core::disk::ActionId::CondaEnvRemove => format!(
                "Remove the conda environment {}? Recreate it with conda create, or from its environment file.",
                render::escaped(listed.identifier.as_deref().unwrap_or_default())
            ),
            ddc_core::disk::ActionId::CondaClean => format!(
                "Clean {}'s package cache? It removes the packages no environment uses, and old downloads.",
                render::escaped(listed.identifier.as_deref().unwrap_or_default())
            ),
            // `free::available` offers no formula: the app removes those.
            ddc_core::disk::ActionId::BrewUninstall | ddc_core::disk::ActionId::BrewUninstallOrphans => continue,
        };
        if prompt::confirm_area_action(&what, std::io::stdin().lock(), &mut out)? {
            chosen.push(listed);
        }
    }
    if chosen.is_empty() {
        writeln!(out, "Nothing confirmed; nothing ran.")?;
        return Ok(None);
    }
    Ok(Some(ActionList { schema_version: FREE_SCHEMA_VERSION, actions: chosen }))
}

fn print(args: &FreeArgs, report: &FreeReport, env: &Env, style: &Style) -> anyhow::Result<()> {
    let mut out = std::io::stdout().lock();
    let written = if args.json {
        serde_json::to_writer_pretty(&mut out, report).map_err(std::io::Error::from).and_then(|()| writeln!(out))
    } else {
        render::free(&mut out, report, env.home(), style)
    };
    match written.and_then(|()| out.flush()) {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => other.map_err(Into::into),
    }
}

fn refuse(why: &str) -> anyhow::Result<ExitCode> {
    eprintln!("ddc disk free: {why}");
    Ok(ExitCode::from(exit::REFUSED))
}
