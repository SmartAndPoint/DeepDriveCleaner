//! `ddc disk free`: an area of the disk report freed by its owning tool
//! (story "Whole disk: free space with the owning tool"; safety ADR,
//! amendment of 2026-10-28). ddc removes no file of an area itself: the
//! tool's own command is the only way it frees space, after the location
//! probe, from a person's confirmation.

pub mod brew;
pub mod conda;
pub(crate) mod measure;
mod model;
pub(crate) mod probe;
mod simctl;

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub use model::*;

use crate::disk::{ActionId, Sources};
use crate::env::Env;
use crate::exec::{AreaActionEntry, AreaJournal, CommandRunner, NATIVE_TIMEOUT, ProcessTable};
use crate::report::after_clean::{self, Remeasured};
use crate::report::{ScanOptions, recheck};
use crate::rules::Catalog;
use crate::store;

/// What a run needs besides the list.
pub struct FreeContext<'a> {
    pub commands: &'a dyn CommandRunner,
    pub sources: &'a Sources,
    pub home: &'a Path,
    /// The skips of `settings.json`: a folder at, above or under one is
    /// not measured.
    pub skips: &'a [PathBuf],
    /// The running processes: a conda environment one uses is refused.
    pub procs: &'a dyn ProcessTable,
    /// Set by a signal handler: actions not yet started do not run, and
    /// the wait for a runtime ends.
    pub cancel: &'a AtomicBool,
    /// The tool's run and, for a runtime, the wait after it.
    pub timeout: Duration,
    /// Between two looks at the runtime list.
    pub poll: Duration,
}

impl<'a> FreeContext<'a> {
    pub fn new(
        commands: &'a dyn CommandRunner,
        sources: &'a Sources,
        home: &'a Path,
        skips: &'a [PathBuf],
        procs: &'a dyn ProcessTable,
        cancel: &'a AtomicBool,
    ) -> Self {
        Self { commands, sources, home, skips, procs, cancel, timeout: NATIVE_TIMEOUT, poll: Duration::from_secs(1) }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

/// Runs each listed action in order, journaling each. A stop leaves the
/// actions not yet started `not-run`, journaled too.
pub fn run(ctx: &FreeContext<'_>, list: &ActionList, journal: &mut dyn AreaJournal) -> io::Result<Vec<ActionReport>> {
    let mut reports = Vec::with_capacity(list.actions.len());
    for listed in &list.actions {
        let report = if ctx.cancelled() {
            not_run(ctx, listed)
        } else {
            match (listed.action, &listed.identifier) {
                (ActionId::SimctlRuntimeDelete, Some(id)) => simctl::run(ctx, id),
                (ActionId::BrewCleanup, _) => brew::run(ctx),
                (ActionId::CondaEnvRemove, Some(id)) => conda::env_run(ctx, id),
                (ActionId::CondaClean, Some(id)) => conda::clean_run(ctx, id),
                // `ActionList::validate` refuses it before anything runs.
                (ActionId::SimctlRuntimeDelete | ActionId::CondaEnvRemove | ActionId::CondaClean, None) => {
                    unreachable!("an action without its identifier")
                }
            }
        };
        journal.record_area(&AreaActionEntry::new(&report))?;
        reports.push(report);
    }
    Ok(reports)
}

/// Each tool's own dry run, after the same probe. Changes nothing and
/// journals nothing.
pub fn dry_run(ctx: &FreeContext<'_>, list: &ActionList) -> Vec<ActionReport> {
    list.actions
        .iter()
        .map(|listed| match (listed.action, &listed.identifier) {
            (ActionId::SimctlRuntimeDelete, Some(id)) => simctl::dry_run(ctx, id),
            (ActionId::BrewCleanup, _) => brew::dry_run(ctx),
            (ActionId::CondaEnvRemove, Some(id)) => conda::env_dry_run(ctx, id),
            (ActionId::CondaClean, Some(id)) => conda::clean_dry_run(ctx, id),
            (ActionId::SimctlRuntimeDelete | ActionId::CondaEnvRemove | ActionId::CondaClean, None) => {
                unreachable!("an action without its identifier")
            }
        })
        .collect()
}

/// The actions the probe would let run now, for the terminal's prompt:
/// each runtime that passes, then Homebrew when its `brew` is there, then
/// each `/opt` conda install with its own `bin/conda`: its package folder,
/// then its environments, as the file system lists them.
pub fn available(ctx: &FreeContext<'_>) -> Vec<ListedAction> {
    let mut out: Vec<ListedAction> = simctl::available(ctx)
        .into_iter()
        .map(|id| ListedAction { action: ActionId::SimctlRuntimeDelete, identifier: Some(id) })
        .collect();
    if brew::available(ctx) {
        out.push(ListedAction { action: ActionId::BrewCleanup, identifier: None });
    }
    for prefix in crate::conda::PREFIXES.iter().map(|p| ctx.sources.opt.join(p)) {
        if ctx.commands.at(&crate::conda::exe(&prefix)).is_none() {
            continue;
        }
        let listed =
            |action, path: &Path| ListedAction { action, identifier: Some(path.to_string_lossy().into_owned()) };
        if prefix.join("pkgs").is_dir() {
            out.push(listed(ActionId::CondaClean, &prefix));
        }
        out.extend(crate::conda::environments(&prefix).iter().map(|e| listed(ActionId::CondaEnvRemove, e)));
    }
    out
}

/// The report of an action a stop kept from starting, its command as the
/// list names it.
fn not_run(ctx: &FreeContext<'_>, listed: &ListedAction) -> ActionReport {
    let id = listed.identifier.as_deref();
    let report = match (listed.action, id) {
        (ActionId::CondaEnvRemove | ActionId::CondaClean, Some(id)) => conda::unprobed(listed.action, id),
        (ActionId::SimctlRuntimeDelete, _) => {
            let args = vec!["runtime".into(), "delete".into(), id.unwrap_or_default().to_owned()];
            report(listed.action, id, &ctx.sources.simctl, &args, &[])
        }
        _ => report(listed.action, id, &ctx.sources.brew, &["cleanup".into()], &brew::BREW_ENV),
    };
    report.with(ActionResult::NotRun)
}

/// A report for the command, before its result.
fn report(
    action: ActionId,
    identifier: Option<&str>,
    exe: &Path,
    args: &[String],
    env: &[(&str, &str)],
) -> ActionReport {
    let mut command = vec![exe.to_string_lossy().into_owned()];
    command.extend(args.iter().cloned());
    let env: Vec<String> = env.iter().map(|(k, v)| format!("{k}={v}")).collect();
    let terminal_command = env.iter().chain(&command).map(|w| quoted(w)).collect::<Vec<_>>().join(" ");
    ActionReport {
        action,
        area: action.area().to_owned(),
        identifier: identifier.map(str::to_owned),
        command,
        env,
        terminal_command,
        result: ActionResult::NotRun,
        bytes_before: None,
        bytes_after: None,
        output: String::new(),
        preview: None,
    }
}

impl ActionReport {
    fn with(mut self, result: ActionResult) -> Self {
        self.result = result;
        self
    }
}

/// `failed`, worded as needing an administrator only when the tool named a
/// permission error. ddc never asks for a password itself.
fn run_failed(mut report: ActionReport, output: &str) -> ActionReport {
    let needs_admin = output.contains("Permission denied") || output.contains("Operation not permitted");
    report.output = output.to_owned();
    report.with(ActionResult::Failed { needs_admin })
}

/// The first words of a stopped tool's `output`: the app words it as a
/// stop, not as the tool's own failure.
pub const STOPPED: &str = "stopped before the tool finished";

/// `failed` after a stop arrived while the tool ran: part of it may be done.
fn stopped(mut report: ActionReport) -> ActionReport {
    report.output = if report.output.is_empty() { STOPPED.to_owned() } else { format!("{STOPPED}\n{}", report.output) };
    report.with(ActionResult::Failed { needs_admin: false })
}

/// One word for a POSIX shell: as is when it is plain, else in single quotes.
fn quoted(word: &str) -> String {
    let plain = !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric() || "_./=:@%+-,".contains(c));
    if plain { word.to_owned() } else { format!("'{}'", word.replace('\'', r"'\''")) }
}

/// Edits `last-disk.json` after the actions that ran: their areas become
/// stale, a runtime that left the list leaves `simulators.runtimes`, and
/// `listed_bytes` is the sum of those that remain; an `/opt` environment
/// conda removed leaves the `conda` section, and one still there after
/// conda exited 0 stays. Every other number stays as measured.
pub fn edit_last_disk(env: &Env, reports: &[ActionReport]) -> ReportEdit {
    let ran: Vec<&ActionReport> = reports.iter().filter(|r| r.ran()).collect();
    if ran.is_empty() {
        return ReportEdit::Unchanged;
    }
    let Ok(Some(mut disk)) = store::read_last_disk(env) else { return ReportEdit::Skipped };
    for r in &ran {
        if let Some(area) = disk.areas.iter_mut().find(|a| a.id == r.area) {
            area.stale = true;
        }
        let gone = matches!(r.result, ActionResult::Freed | ActionResult::RemovedNotFreed);
        if let (true, ActionId::SimctlRuntimeDelete, Some(id), Some(sims)) =
            (gone, r.action, &r.identifier, disk.simulators.as_mut())
        {
            sims.runtimes.retain(|rt| &rt.identifier != id);
            sims.listed_bytes = sims.runtimes.iter().filter_map(|rt| rt.size_bytes).sum();
        }
        if let (ActionResult::Freed, ActionId::CondaEnvRemove, Some(id), Some(conda)) =
            (&r.result, r.action, &r.identifier, disk.conda.as_mut())
        {
            // Listed by its identifier, given to conda as its `-p` argument.
            let given = r.command.iter().skip_while(|a| *a != "-p").nth(1);
            for install in &mut conda.installs {
                install.environments.retain(|e| {
                    let path = e.path.to_string_lossy();
                    path != id.as_str() && given.is_none_or(|g| path != g.as_str())
                });
            }
        }
    }
    match store::write_last_disk(env, &disk) {
        Ok(_) => ReportEdit::Edited,
        Err(_) => ReportEdit::Failed,
    }
}

/// After a `brew-cleanup` that ran, measures the `cache.homebrew` finding
/// of `last-scan.json` again, as a clean does for an item a tool cleaned,
/// when that report has the current schema and the home's roots.
pub fn edit_last_scan(env: &Env, opts: &ScanOptions, reports: &[ActionReport]) -> ReportEdit {
    if !reports.iter().any(|r| r.action == ActionId::BrewCleanup && r.ran()) {
        return ReportEdit::Unchanged;
    }
    let Some(mut scan) = store::read_last_scan(env).ok().flatten().filter(|r| recheck::same_roots(r, env, opts)) else {
        return ReportEdit::Skipped;
    };
    let catalog = Catalog::builtin(env.home());
    let remeasured: HashMap<PathBuf, Remeasured> = scan
        .findings
        .iter()
        .filter(|f| f.rule == "cache.homebrew")
        .filter(|f| !opts.user_skips.iter().any(|s| f.path.starts_with(s) || s.starts_with(&f.path)))
        .map(|f| (f.path.clone(), after_clean::remeasure(&catalog, &f.rule, &f.path, &opts.walk)))
        .collect();
    if remeasured.is_empty() {
        return ReportEdit::Unchanged;
    }
    after_clean::edit_tool_cleaned(&mut scan, &remeasured, opts);
    match store::write_last_scan(env, &scan) {
        Ok(_) => ReportEdit::Edited,
        Err(_) => ReportEdit::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::quoted;

    #[test]
    fn terminal_words_are_quoted_only_when_needed() {
        assert_eq!(quoted("HOMEBREW_NO_AUTOREMOVE=1"), "HOMEBREW_NO_AUTOREMOVE=1");
        assert_eq!(quoted("/Applications/Xcode.app/Contents/Developer"), "/Applications/Xcode.app/Contents/Developer");
        assert_eq!(quoted("DEVELOPER_DIR=/Applications/Xcode 27.app"), "'DEVELOPER_DIR=/Applications/Xcode 27.app'");
        assert_eq!(quoted("it's"), r"'it'\''s'");
        assert_eq!(quoted(""), "''");
    }
}
