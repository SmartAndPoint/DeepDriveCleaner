//! `brew cleanup`, with no arguments, from the Homebrew area's own `brew`.
//! It removes old versions and downloads, never a formula: without
//! `HOMEBREW_NO_AUTOREMOVE` it would also uninstall dependencies nothing
//! needs.

use std::path::PathBuf;

use super::model::{ActionReport, ActionResult, Preview};
use super::{FreeContext, finish, measure, probe, report, run_failed};
use crate::brew::Budget;
use crate::disk::ActionId;

/// Every `brew` call's environment, the probe's and the dry run's included,
/// so the preview and the run follow the same rules.
pub const BREW_ENV: [(&str, &str); 3] =
    [("HOMEBREW_NO_AUTOREMOVE", "1"), ("HOMEBREW_NO_ENV_HINTS", "1"), ("NO_COLOR", "1")];

fn command(exe: &std::path::Path) -> ActionReport {
    report(ActionId::BrewCleanup, None, exe, &["cleanup".to_owned()], &BREW_ENV)
}

/// The area's `brew` exists and the area's folder is there; the probe
/// decides the rest.
pub(super) fn available(ctx: &FreeContext<'_>) -> bool {
    ctx.commands.at(&ctx.sources.brew).is_some() && ctx.sources.data_root.join("opt/homebrew").is_dir()
}

pub(super) fn dry_run(ctx: &FreeContext<'_>) -> ActionReport {
    let report = command(&ctx.sources.brew);
    let target = match probe::brew(ctx.commands, ctx.sources, ctx.home, &BREW_ENV, &Budget::new(ctx.timeout)) {
        Ok(t) => t,
        Err(reason) => return report.with(ActionResult::Refused { reason }),
    };
    let mut report = command(&target.exe);
    report.bytes_before = measure::allocated_all(&folders(&target), ctx.skips);
    let args = ["cleanup".to_owned(), "-n".to_owned()];
    match ctx.commands.run(&target.exe, &args, &BREW_ENV, ctx.timeout) {
        Ok(o) if o.success => {
            let lines: Vec<String> = o.stdout.lines().map(str::to_owned).collect();
            let estimate_bytes = lines.iter().find_map(|l| estimate(l));
            report.preview = Some(Preview { lines, estimate_bytes });
            report.with(ActionResult::WouldRun)
        }
        Ok(o) => run_failed(report, &crate::exec::tail(&o.stdout, &o.stderr)),
        Err(e) => run_failed(report, &e.to_string()),
    }
}

pub(super) fn run(ctx: &FreeContext<'_>) -> ActionReport {
    let report = command(&ctx.sources.brew);
    let target = match probe::brew(ctx.commands, ctx.sources, ctx.home, &BREW_ENV, &Budget::new(ctx.timeout)) {
        Ok(t) => t,
        Err(reason) => return report.with(ActionResult::Refused { reason }),
    };
    let mut report = command(&target.exe);
    let folders = folders(&target);
    report.bytes_before = measure::allocated_all(&folders, ctx.skips);
    if ctx.cancelled() {
        return report.with(ActionResult::NotRun);
    }
    let ran = ctx.commands.run(&target.exe, &report.command[1..], &BREW_ENV, ctx.timeout);
    report.bytes_after = measure::allocated_all(&folders, ctx.skips);
    finish(ctx, report, ran, ActionResult::Freed, ctx.timeout)
}

/// The two places it frees: the Cellar and the download cache.
fn folders(target: &probe::BrewTarget) -> Vec<PathBuf> {
    vec![target.cellar.clone(), target.cache.clone()]
}

/// `==> This operation would free approximately 1.2GB of disk space.`, read
/// leniently: Homebrew's units are powers of 1024.
fn estimate(line: &str) -> Option<u64> {
    let rest = line.split("would free approximately").nth(1)?.trim_start();
    let size = rest.split_whitespace().next()?;
    let split = size.find(|c: char| !(c.is_ascii_digit() || c == '.'))?;
    let (number, unit) = size.split_at(split);
    let number: f64 = number.parse().ok()?;
    let scale: u64 = match unit {
        "B" => 1,
        "KB" => 1 << 10,
        "MB" => 1 << 20,
        "GB" => 1 << 30,
        "TB" => 1 << 40,
        _ => return None,
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
    Some((number * scale as f64) as u64)
}

#[cfg(test)]
mod tests {
    use super::estimate;

    #[test]
    fn the_estimate_is_read_from_brews_last_line() {
        assert_eq!(estimate("==> This operation would free approximately 1.5GB of disk space."), Some(3 << 29));
        assert_eq!(estimate("This operation would free approximately 12KB of disk space."), Some(12 << 10));
        assert_eq!(estimate("This operation would free approximately 100B of disk space."), Some(100));
        assert_eq!(estimate("Would remove: /opt/homebrew/Cellar/x/1.0 (12 files, 1.2MB)"), None);
        assert_eq!(estimate("This operation would free approximately lots of disk space."), None);
    }
}
