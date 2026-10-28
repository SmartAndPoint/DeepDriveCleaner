//! `ddc clean --items`: clean exactly the items a sheet confirmed (story "CLI:
//! clean an explicit item list"; ADR "Menu bar app: Swift shell over the ddc
//! CLI", §6 and §7). The list is the selection and the consent: no class
//! filter, no default selection, no prompt, no terminal needed.

use std::collections::HashMap;
use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Context as _;
use ddc_core::env::Env;
use ddc_core::exec::preflight::{self, PreflightPorts};
use ddc_core::exec::report::CleanReport;
use ddc_core::exec::{
    self, Consent, ExecOptions, ExecProgress, ExecReport, FileJournal, ItemReport, Outcome, Ports, SystemCommands,
    SystemProcesses, SystemTrash,
};
use ddc_core::model::ScanReport;
use ddc_core::plan::{ListedItem, ListedItems, Refused, plan_listed};
use ddc_core::progress::{Phase, WalkStep};
use ddc_core::report::after_clean::{self, Remeasured};
use ddc_core::report::contents::EntryIndex;
use ddc_core::report::recheck::{self, RecheckError, Walk};
use ddc_core::report::volumes::SystemStatFs;
use ddc_core::report::{self, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::scan::{Progress, WalkOptions};
use ddc_core::settings::ScanFlags;
use ddc_core::store::{self, LockMode};
use ddc_core::sys::bulk::BulkReader;
use ddc_core::sys::signals;

use crate::cli::CleanArgs;
use crate::render::{self, Style};
use crate::{exit, progress, prompt};

pub fn run(args: &CleanArgs, items: &Path) -> anyhow::Result<ExitCode> {
    // Refused before the lock, the data directory or the journal is touched.
    let bytes = match super::read_list(items) {
        Ok(bytes) => bytes,
        Err(e) => return refuse(&format!("cannot read {}: {e}", items.display())),
    };
    let list = match ListedItems::parse(&bytes) {
        Ok(list) => list,
        Err(e) => return refuse(&e.to_string()),
    };
    let env = Env::from_process(args.data_dir.clone())?;
    let resolved = super::resolve(
        &env,
        ScanFlags {
            paths: args.paths.clone(),
            excludes: args.exclude.clone(),
            skips: args.skip.clone(),
            min_age_days: args.min_age,
        },
    )?;
    super::note_missing(&resolved.missing);
    // Nobody can be asked here; the dry run refuses too, so the sheet predicts the clean.
    if !resolved.dropped_excludes.is_empty() {
        return refuse(&prompt::dropped_excludes(&resolved.dropped_excludes, env.home()));
    }
    let catalog = Catalog::builtin(env.home());
    if args.dry_run {
        return dry_run(args, &env, &catalog, &list, &resolved.options);
    }

    let _lock = store::try_lock(&env, LockMode::Exclusive)?;
    let reporter = progress::Reporter::new(args.progress.is_some());
    reporter.phase(Phase::Preparing);
    let opts = ScanOptions { walk: WalkOptions::default(), ..resolved.options };
    // A skipped folder is never read, so no re-check sees it: an item at, under
    // or above one is kept, as the dry run's preflight says (safety ADR,
    // amendment of 2026-10-28).
    let (skipped, items): (Vec<ListedItem>, Vec<ListedItem>) = list
        .items
        .iter()
        .cloned()
        .partition(|i| opts.user_skips.iter().any(|s| i.path.starts_with(s) || s.starts_with(&i.path)));
    // Read once under the lock: the re-check's estimate, and the report this
    // clean edits after it runs (story "Faster clean: re-check only what the
    // selection needs"). A report of other roots is not that report.
    let previous = store::read_last_scan(&env).ok().flatten().filter(|r| recheck::same_roots(r, &env, &opts));
    // Another root's file count says nothing about this walk.
    let disk_files = previous.as_ref().map(|r| r.totals.files);
    let full = |expected: Option<u64>| -> anyhow::Result<(ScanReport, EntryIndex)> {
        // Only a whole-tree walk sees toolchain pins and new claims anywhere.
        let scan_progress = Progress::default();
        let (scanned, entries) = reporter.walk(&scan_progress, "scanning", WalkStep::Recheck, expected, || {
            report::scan_with_entries(&env, &opts, &scan_progress, &SystemStatFs)
        })?;
        reporter.walk_done(WalkStep::Recheck, &scanned.totals, expected);
        Ok((scanned, entries))
    };
    // `base` is the report edited after the clean; `None`: the rescan's.
    let (scanned, entries, base) = match recheck::walk_for(&items, &catalog, previous.as_ref()) {
        Walk::Pruned => {
            let expected = previous.as_ref().and_then(|p| recheck::expected_files(p, &items, &catalog));
            let scan_progress = Progress::default();
            let checked = reporter.walk(&scan_progress, "checking", WalkStep::Recheck, expected, || {
                recheck::pruned(&env, &opts, &catalog, &items, &scan_progress, &SystemStatFs, None)
            });
            match checked {
                Ok((scanned, entries)) => {
                    reporter.walk_done(WalkStep::Recheck, &scanned.totals, expected);
                    (scanned, entries, previous)
                }
                // The full walk cannot read it either: the item ends `changed-since-scan` there too.
                Err(RecheckError::UnreadableAncestor(_)) => {
                    let (scanned, entries) = full(disk_files)?;
                    (scanned, entries, None)
                }
                Err(RecheckError::Scan(e)) => return Err(e.into()),
            }
        }
        Walk::Full(_) => {
            let (scanned, entries) = full(disk_files)?;
            (scanned, entries, None)
        }
    };
    // A picked entry is checked against its folder's full list, not the report's top 100.
    let mut listed = plan_listed(&scanned, &entries, &catalog, &items);
    listed.refused.extend(skipped.into_iter().map(|item| Refused { item, reason: exec::SkipReason::Kept }));

    let mut journal = FileJournal::open(&env).context("cannot open the action journal")?;
    let journal_path = journal.path.clone();
    let cancel = signals::install().context("cannot install the signal handler")?;
    let commands = SystemCommands::new(&env);
    let exec_progress = ExecProgress::default();
    let result = reporter.clean(&exec_progress, env.home(), cancel, |events| {
        exec::execute(
            &env,
            &catalog,
            &listed.plan,
            &Consent::Listed,
            Ports { commands: &commands, trash: &SystemTrash, procs: &SystemProcesses, journal: &mut journal },
            &ExecOptions { cancel, progress: Some(&exec_progress), events, ..ExecOptions::default() },
        )
    })?;
    let mut clean = CleanReport::new(&list.items, &listed.refused, &result, journal_path.clone());
    // `last-scan.json` follows what was done, after a cancel too.
    // An older report takes the re-check's findings for the items not touched.
    let (mut base, fresh) = match base {
        Some(previous) => (previous, Some(scanned)),
        None => (scanned, None),
    };
    let remeasured: HashMap<PathBuf, Remeasured> = clean
        .items
        .iter()
        .filter(|i| matches!(i.outcome, Outcome::Native { .. }))
        .map(|i| (i.path.clone(), after_clean::remeasure(&catalog, &i.rule, &i.path, &opts.walk)))
        .collect();
    after_clean::edit(&mut base, &clean, fresh.as_ref(), &remeasured, &opts);
    clean.last_scan_saved = match store::write_last_scan(&env, &base) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("ddc: warning: cannot save the scan report: {e}");
            false
        }
    };

    let mut out = std::io::stdout().lock();
    if args.json {
        serde_json::to_writer_pretty(&mut out, &clean)?;
        writeln!(out)?;
    } else {
        let style = Style { color: render::use_color(std::io::stdout().is_terminal(), Env::no_color()), all: false };
        // Refused items are skipped ones too: counted and listed in list order with the rest.
        let shown = ExecReport {
            items: clean
                .items
                .iter()
                .map(|i| ItemReport {
                    path: i.path.clone(),
                    rule: i.rule.clone(),
                    class: i.class,
                    action: i.action.as_str(),
                    bytes: i.bytes.unwrap_or(0),
                    outcome: i.outcome.clone(),
                })
                .collect(),
            ..result.clone()
        };
        prompt::print_result(&mut out, &shown, env.home(), &journal_path, &style)?;
    }
    out.flush()?;
    // A signal stopped the run (or arrived after it): end by it, so the caller
    // sees a cancel, with the report already on stdout.
    if let Some(sig) = signals::received() {
        signals::reraise(sig);
    }
    // With --json the report carries failures; the run itself succeeded. As
    // plain `clean`, the text mode fails on an item that failed.
    let failed = result.items.iter().any(|i| matches!(i.outcome, Outcome::Failed { .. }));
    Ok(if !args.json && failed { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}

/// One JSON line per item as its checks finish, then a summary line. Read-only:
/// no rescan, no walk outside the listed items, no Trash, journal or tool run.
fn dry_run(
    args: &CleanArgs,
    env: &Env,
    catalog: &Catalog,
    list: &ListedItems,
    opts: &ScanOptions,
) -> anyhow::Result<ExitCode> {
    let _lock = store::try_lock(env, LockMode::Shared)?;
    let commands = SystemCommands::new(env);
    let ports = PreflightPorts { commands: &commands, procs: &SystemProcesses, reader: &BulkReader };
    let mut out = std::io::stdout().lock();
    let home = env.home().to_path_buf();
    preflight::preflight(env, catalog, &opts.roots, &opts.excludes, &list.items, &ports, &mut |line| {
        if args.json {
            serde_json::to_writer(&mut out, line)?;
            writeln!(out)?;
        } else if let preflight::PreflightLine::Item(item) = line {
            writeln!(out, "{}  {}", prediction_text(&item.prediction), render::tilde(&item.path, &home))?;
        }
        out.flush()
    })?;
    Ok(ExitCode::SUCCESS)
}

fn prediction_text(p: &preflight::Prediction) -> String {
    match p {
        preflight::Prediction::WouldDelete => "would delete".to_owned(),
        preflight::Prediction::WouldTrash => "would move to Trash".to_owned(),
        preflight::Prediction::WouldRun => "would run its tool".to_owned(),
        preflight::Prediction::WouldSkip(why) => format!("would skip ({})", prompt::reason(why)),
    }
}

fn refuse(why: &str) -> anyhow::Result<ExitCode> {
    eprintln!("ddc clean: {why}");
    Ok(ExitCode::from(exit::REFUSED))
}
