//! The `/opt` conda installs' own commands (story "Whole disk: Conda
//! environments and its package cache, with conda"; safety ADR, amendment
//! of 2026-10-29): `conda env remove -p` of one environment, and `conda
//! clean` of the install's package folder limited to it by
//! `CONDA_PKGS_DIRS`. Each runs from the install's own `bin/conda`, after
//! the probe, and its dry run asks the same probe.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::model::{ActionReport, ActionResult, Preview};
use super::{FreeContext, finish, measure, probe, report, run_failed};
use crate::conda::{CLEAN, CLEAN_DRY_RUN, ENV_REMOVE, PKGS_DIRS_VAR, exe, first_json};
use crate::disk::ActionId;

/// `conda env remove -p <path> --yes`, by `exe`.
pub(super) fn env_command(identifier: &str, exe: &Path, path: &str) -> ActionReport {
    let args: Vec<String> = ENV_REMOVE.iter().map(|a| a.replace("{path}", path)).collect();
    report(ActionId::CondaEnvRemove, Some(identifier), exe, &args, &[])
}

/// `CONDA_PKGS_DIRS=<pkgs> conda clean …`, by `exe`.
pub(super) fn clean_command(identifier: &str, exe: &Path, pkgs: &str) -> ActionReport {
    let args: Vec<String> = CLEAN.iter().map(|a| (*a).to_owned()).collect();
    report(ActionId::CondaClean, Some(identifier), exe, &args, &[(PKGS_DIRS_VAR, pkgs)])
}

/// The command as it would run before the probe has spoken: the
/// identifier's own `bin/conda`, for a refusal or a stop.
pub(super) fn unprobed(action: ActionId, identifier: &str) -> ActionReport {
    let id = Path::new(identifier);
    if action == ActionId::CondaEnvRemove {
        let prefix = id.parent().and_then(Path::parent).unwrap_or(id);
        env_command(identifier, &exe(prefix), identifier)
    } else {
        clean_command(identifier, &exe(id), &id.join("pkgs").to_string_lossy())
    }
}

pub(super) fn env_dry_run(ctx: &FreeContext<'_>, identifier: &str) -> ActionReport {
    let target = match probe::conda_environment(ctx.commands, ctx.sources, ctx.home, ctx.procs, Path::new(identifier)) {
        Ok(t) => t,
        Err(reason) => return unprobed(ActionId::CondaEnvRemove, identifier).with(ActionResult::Refused { reason }),
    };
    let mut report = env_command(identifier, &target.exe, &target.spelled);
    report.bytes_before = measure::own(&target.path, ctx.skips);
    report.preview = Some(Preview { lines: Vec::new(), estimate_bytes: report.bytes_before });
    report.with(ActionResult::WouldRun)
}

pub(super) fn env_run(ctx: &FreeContext<'_>, identifier: &str) -> ActionReport {
    let target = match probe::conda_environment(ctx.commands, ctx.sources, ctx.home, ctx.procs, Path::new(identifier)) {
        Ok(t) => t,
        Err(reason) => return unprobed(ActionId::CondaEnvRemove, identifier).with(ActionResult::Refused { reason }),
    };
    let mut report = env_command(identifier, &target.exe, &target.spelled);
    report.bytes_before = measure::own(&target.path, ctx.skips);
    if ctx.cancelled() {
        return report.with(ActionResult::NotRun);
    }
    let ran = ctx.commands.run(&target.exe, &report.command[1..], &[], ctx.timeout);
    report.bytes_after = measure::own(&target.path, ctx.skips);
    // conda can exit 0 having removed nothing (an environment with no
    // package records, a folder it cannot write): still there is not freed.
    let still_there = std::fs::symlink_metadata(&target.path).is_ok();
    let done = if still_there { ActionResult::RemovedNotFreed } else { ActionResult::Freed };
    finish(ctx, report, ran, done, ctx.timeout)
}

/// What `conda clean --dry-run --json` says it would take: the sizes of the
/// packages and the tarballs. The index cache and the logs are listed as
/// bare paths, with no size.
#[derive(Deserialize)]
struct CleanPlan {
    packages: Option<Sized>,
    tarballs: Option<Sized>,
}

#[derive(Deserialize)]
struct Sized {
    total_size: u64,
}

/// The estimate of a dry run's output: its first JSON document; what
/// follows it (`DryRunExit`) is ignored.
pub(crate) fn estimate(stdout: &str) -> Option<u64> {
    let plan: CleanPlan = first_json(stdout)?;
    Some(plan.packages.map_or(0, |s| s.total_size) + plan.tarballs.map_or(0, |s| s.total_size))
}

pub(super) fn clean_dry_run(ctx: &FreeContext<'_>, identifier: &str) -> ActionReport {
    let target = match probe::conda_packages(ctx.commands, ctx.sources, ctx.home, Path::new(identifier)) {
        Ok(t) => t,
        Err(reason) => return unprobed(ActionId::CondaClean, identifier).with(ActionResult::Refused { reason }),
    };
    let mut report = clean_command(identifier, &target.exe, &target.spelled);
    report.bytes_before = measure::allocated(&target.path, ctx.skips);
    let args: Vec<String> = CLEAN_DRY_RUN.iter().map(|a| (*a).to_owned()).collect();
    match ctx.commands.run(&target.exe, &args, &[(PKGS_DIRS_VAR, &target.spelled)], ctx.timeout) {
        Ok(o) if o.success => {
            let lines: Vec<String> = o.stdout.lines().map(str::to_owned).collect();
            report.preview = Some(Preview { lines, estimate_bytes: estimate(&o.stdout) });
            report.with(ActionResult::WouldRun)
        }
        Ok(o) => run_failed(report, &crate::exec::tail(&o.stdout, &o.stderr)),
        Err(e) => run_failed(report, &e.to_string()),
    }
}

pub(super) fn clean_run(ctx: &FreeContext<'_>, identifier: &str) -> ActionReport {
    let target = match probe::conda_packages(ctx.commands, ctx.sources, ctx.home, Path::new(identifier)) {
        Ok(t) => t,
        Err(reason) => return unprobed(ActionId::CondaClean, identifier).with(ActionResult::Refused { reason }),
    };
    let mut report = clean_command(identifier, &target.exe, &target.spelled);
    let folders: [PathBuf; 1] = [target.path.clone()];
    report.bytes_before = measure::allocated_all(&folders, ctx.skips);
    if ctx.cancelled() {
        return report.with(ActionResult::NotRun);
    }
    let ran = ctx.commands.run(&target.exe, &report.command[1..], &[(PKGS_DIRS_VAR, &target.spelled)], ctx.timeout);
    report.bytes_after = measure::allocated_all(&folders, ctx.skips);
    finish(ctx, report, ran, ActionResult::Freed, ctx.timeout)
}

#[cfg(test)]
mod tests {
    use super::estimate;

    #[test]
    fn the_estimate_sums_the_packages_and_the_tarballs_of_the_first_document() {
        let out = r#"{"index_cache": ["/opt/miniconda3/pkgs/cache"], "packages": {"pkg_sizes": {}, "total_size": 1000},
"tarballs": {"pkg_sizes": {}, "total_size": 234}, "logfiles": []}
DryRunExit: Dry run. Exiting.
"#;
        assert_eq!(estimate(out), Some(1234));
        assert_eq!(estimate(r#"{"index_cache": []}"#), Some(0));
        assert_eq!(estimate("DryRunExit: Dry run. Exiting."), None);
    }
}
