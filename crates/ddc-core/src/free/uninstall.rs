//! `brew uninstall --formula` from the Homebrew area's own `brew` (story
//! "Whole disk: Homebrew packages, their sizes, and brew uninstall"; safety
//! ADR, amendment of 2026-10-29): one formula, every keg of it, or the
//! orphans a removal left. Formulae only, never a cask of the same name.
//!
//! Right before the command, ddc reads `brew info` (and `brew services
//! list` when brew has it) again and refuses when a confirmed formula is
//! gone, pinned, has other kegs than the confirmed ones, is needed, is no
//! longer an orphan, or has a service job. brew has no dry run of its own:
//! the dry run is that re-read, with the kegs measured.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::model::{ActionReport, ActionResult, KegReport, ListedAction, Preview, Refusal};
use super::{FreeContext, NOT_STARTED, finish, measure, probe, report};
use crate::brew::{self, AskError, Budget, Graph, NeededBy, Service, Services};
use crate::disk::ActionId;

/// The removal, of formulae only.
pub const UNINSTALL: [&str; 2] = ["uninstall", "--formula"];

/// Every keg of each named formula: for a formula with several kegs, and
/// for the orphans. It skips brew's pinned check, which the re-read does.
pub const FORCE: &str = "--force";

/// What a person confirmed.
#[derive(Debug, Clone, Copy)]
pub(super) enum Confirmed<'a> {
    /// One formula by its `full_name`, with the versions of its kegs.
    Formula { full_name: &'a str, kegs: &'a [String] },
    /// The orphans, by `full_name`.
    Orphans(&'a [String]),
}

impl<'a> Confirmed<'a> {
    /// `None` for an action that is no removal of a formula.
    pub(super) fn of(listed: &'a ListedAction) -> Option<Self> {
        match listed.action {
            ActionId::BrewUninstall => Some(Confirmed::Formula {
                full_name: listed.identifier.as_deref().unwrap_or_default(),
                kegs: listed.kegs.as_deref().unwrap_or_default(),
            }),
            ActionId::BrewUninstallOrphans => Some(Confirmed::Orphans(listed.names.as_deref().unwrap_or_default())),
            _ => None,
        }
    }

    fn action(self) -> ActionId {
        match self {
            Confirmed::Formula { .. } => ActionId::BrewUninstall,
            Confirmed::Orphans(_) => ActionId::BrewUninstallOrphans,
        }
    }

    fn names(self) -> Vec<String> {
        match self {
            Confirmed::Formula { full_name, .. } => vec![full_name.to_owned()],
            Confirmed::Orphans(names) => names.to_vec(),
        }
    }

    /// `uninstall --formula [--force] <full_name>…`.
    fn args(self) -> Vec<String> {
        let force = match self {
            Confirmed::Formula { kegs, .. } => kegs.len() > 1,
            Confirmed::Orphans(_) => true,
        };
        let mut args: Vec<String> = UNINSTALL.iter().map(|a| (*a).to_owned()).collect();
        if force {
            args.push(FORCE.to_owned());
        }
        args.extend(self.names());
        args
    }
}

/// The command by `exe`, under brew's four variables.
pub(super) fn command(confirmed: Confirmed<'_>, exe: &Path) -> ActionReport {
    let identifier = match confirmed {
        Confirmed::Formula { full_name, .. } => Some(full_name),
        Confirmed::Orphans(_) => None,
    };
    report(confirmed.action(), identifier, exe, &confirmed.args(), &brew::ENV)
}

/// What the re-read let through; its kegs are in the report.
struct Checked {
    /// The area's `brew`, as the probe resolved it.
    exe: PathBuf,
    /// `<cellar>/<name>` of each confirmed formula: what is measured
    /// before and after.
    racks: Vec<PathBuf>,
}

/// `failed` with brew's command not started; never worded as needing an
/// administrator, so the sheet offers no Terminal command for a `brew
/// info` that could not run.
pub(super) fn not_started(mut report: ActionReport, detail: &str) -> ActionReport {
    report.output = if detail.is_empty() { NOT_STARTED.to_owned() } else { format!("{NOT_STARTED}\n{detail}") };
    report.with(ActionResult::Failed { needs_admin: false })
}

/// The probe and the re-read, within `budget`: the command by the
/// area's `brew` with every keg it would take, measured, and what the run
/// needs; or, with `None`, the report of why not.
fn check(ctx: &FreeContext<'_>, confirmed: Confirmed<'_>, budget: &Budget) -> (ActionReport, Option<Checked>) {
    let target = match probe::brew(ctx.commands, ctx.sources, ctx.home, &brew::ENV, budget) {
        Ok(t) => t,
        Err(reason) => return (command(confirmed, &ctx.sources.brew).with(ActionResult::Refused { reason }), None),
    };
    let mut report = command(confirmed, &target.exe);
    let installed = match brew::ask_info(ctx.commands, &target.exe, budget.remaining()) {
        Ok(installed) => installed,
        Err(AskError::Failed(why)) => return (not_started(report, &why), None),
        Err(AskError::TimedOut) => {
            let why = format!("brew info timed out after {} s", budget.total().as_secs());
            return (not_started(report, &why), None);
        }
    };
    let services = brew::ask_services(ctx.commands, &target.exe, &target.prefix, budget.remaining());
    if services == Services::Unreadable {
        return (not_started(report, "brew services list failed or printed what ddc cannot read"), None);
    }
    let graph = Graph::new(&installed);
    let set = match judge(&graph, &services, confirmed, ctx.home) {
        Ok(set) => set,
        Err((reason, needed_by)) => {
            report.needed_by = needed_by;
            return (report.with(ActionResult::Refused { reason }), None);
        }
    };
    let nodes = graph.nodes();
    let mut kegs = Vec::new();
    for &i in &set {
        for version in &nodes[i].versions {
            kegs.push(KegReport {
                formula: nodes[i].full_name.clone(),
                version: version.clone(),
                path: target.cellar.join(&nodes[i].name).join(version),
                bytes: None,
            });
        }
    }
    let paths: Vec<PathBuf> = kegs.iter().map(|k| k.path.clone()).collect();
    for (keg, bytes) in kegs.iter_mut().zip(measure::regions(&paths, ctx.skips)) {
        keg.bytes = bytes;
    }
    let racks: Vec<PathBuf> = set.iter().map(|&i| target.cellar.join(&nodes[i].name)).collect();
    report.bytes_before = measure::allocated_all(&racks, ctx.skips);
    report.kegs = kegs;
    (report, Some(Checked { exe: target.exe, racks }))
}

/// The confirmed formulae in the fresh listing, by `full_name`, or why
/// they may not be removed, with what needs them.
fn judge(
    graph: &Graph,
    services: &Services,
    confirmed: Confirmed<'_>,
    home: &Path,
) -> Result<Vec<usize>, (Refusal, Vec<NeededBy>)> {
    // `ActionList::validate` refuses it first; never a nameless uninstall.
    if confirmed.names().is_empty() {
        return Err((Refusal::BrewNotInstalled, Vec::new()));
    }
    // A formula listed without what it needs could be one that needs these.
    if !graph.incomplete.is_empty() {
        return Err((Refusal::BrewFieldMissing, Vec::new()));
    }
    let set: Vec<usize> = confirmed
        .names()
        .iter()
        .map(|name| graph.find(name).ok_or((Refusal::BrewNotInstalled, Vec::new())))
        .collect::<Result<_, _>>()?;
    let nodes = graph.nodes();
    if set.iter().any(|&i| nodes[i].pinned) {
        return Err((Refusal::BrewPinned, Vec::new()));
    }
    match confirmed {
        Confirmed::Formula { kegs, .. } => {
            let now: BTreeSet<&String> = nodes[set[0]].versions.iter().collect();
            if now != kegs.iter().collect::<BTreeSet<_>>() {
                return Err((Refusal::BrewKegsChanged, Vec::new()));
            }
        }
        Confirmed::Orphans(_) => {
            if set.iter().any(|&i| nodes[i].on_request) {
                return Err((Refusal::BrewNotOrphan, Vec::new()));
            }
        }
    }
    let leaving: BTreeSet<usize> = set.iter().copied().collect();
    let needed: Vec<NeededBy> = set.iter().map(|&i| graph.needed_by(i, &leaving)).filter(|n| !n.is_empty()).collect();
    if !needed.is_empty() {
        return Err((Refusal::BrewNeeded, needed));
    }
    for &i in &set {
        let reason = match services.of(&nodes[i].name, home) {
            Service::Running => Refusal::BrewServiceRunning,
            Service::Root => Refusal::BrewServiceRoot,
            Service::Registered => Refusal::BrewServiceRegistered,
            Service::None | Service::NotKnown | Service::Unreadable => continue,
        };
        return Err((reason, Vec::new()));
    }
    Ok(set)
}

/// The re-read without the command: what would be removed, every keg
/// measured, or the refusal.
pub(super) fn dry_run(ctx: &FreeContext<'_>, confirmed: Confirmed<'_>) -> ActionReport {
    let (mut report, checked) = check(ctx, confirmed, &Budget::new(ctx.timeout));
    if checked.is_none() {
        return report;
    }
    let lines = report.kegs.iter().map(|k| k.path.to_string_lossy().into_owned()).collect();
    report.preview = Some(Preview { lines, estimate_bytes: report.bytes_before });
    report.with(ActionResult::WouldRun)
}

/// The re-read, then the command within what is left of the timeout:
/// `freed` when every confirmed keg is gone, `removed-not-freed` when brew
/// exited 0 and one is still there.
pub(super) fn run(ctx: &FreeContext<'_>, confirmed: Confirmed<'_>) -> ActionReport {
    let budget = Budget::new(ctx.timeout);
    let (mut report, checked) = check(ctx, confirmed, &budget);
    let Some(checked) = checked else { return report };
    let left = budget.remaining();
    if left < ctx.floor {
        let why =
            format!("the check took {} s of {} s; try again", budget.elapsed().as_secs(), budget.total().as_secs());
        return not_started(report, &why);
    }
    if ctx.cancelled() {
        return report.with(ActionResult::NotRun);
    }
    let ran = ctx.commands.run(&checked.exe, &report.command[1..], &brew::ENV, left);
    report.bytes_after = measure::allocated_all(&checked.racks, ctx.skips);
    let still_there = report.kegs.iter().any(|k| std::fs::symlink_metadata(&k.path).is_ok());
    let done = if still_there { ActionResult::RemovedNotFreed } else { ActionResult::Freed };
    finish(ctx, report, ran, done, left)
}
