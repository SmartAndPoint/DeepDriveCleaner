//! `simctl runtime delete <identifier>`: one simulator runtime, through
//! CoreSimulator's own `simctl`, found as `ddc disk` finds it. The
//! identifier is the first argument after `delete`, followed only by
//! `--dry-run` in the dry run; nothing that keeps the asset is ever passed.

use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::model::{ActionReport, ActionResult, Preview, Refusal};
use super::{FreeContext, measure, probe, report, run_failed, stopped};
use crate::disk::ActionId;
use crate::disk::simulators::{self, RawDevice, RawRuntime, Simctl, TOOL_TIMEOUT, Unready};

/// The command for `identifier`, run or not: `simctl`'s path as found,
/// else where it would be.
fn command(ctx: &FreeContext<'_>, simctl: Option<&Simctl>, identifier: &str) -> ActionReport {
    let exe = simctl.map_or(&ctx.sources.simctl, |s| &s.exe);
    let args = vec!["runtime".to_owned(), "delete".to_owned(), identifier.to_owned()];
    let env: Vec<(&str, &str)> = simctl.map(|s| s.env().to_vec()).unwrap_or_default();
    report(ActionId::SimctlRuntimeDelete, Some(identifier), exe, &args, &env)
}

struct Listing {
    simctl: Simctl,
    runtimes: Vec<RawRuntime>,
    devices: Option<BTreeMap<String, Vec<RawDevice>>>,
}

fn list(ctx: &FreeContext<'_>) -> Result<Listing, Refusal> {
    let simctl = simulators::locate(ctx.commands, ctx.sources).map_err(|e| match e {
        Unready::NoDeveloperTools => Refusal::NoDeveloperTools,
        Unready::FirstLaunchPending => Refusal::FirstLaunchPending,
        Unready::Missing => Refusal::SimctlFailed,
    })?;
    let runtimes = runtimes(ctx, &simctl).ok_or(Refusal::SimctlFailed)?;
    let devices = ctx
        .commands
        .run(&simctl.exe, &args(&["list", "devices", "-j"]), &simctl.env(), TOOL_TIMEOUT)
        .ok()
        .filter(|o| o.success)
        .and_then(|o| simulators::parse_devices(&o.stdout));
    Ok(Listing { simctl, runtimes, devices })
}

fn runtimes(ctx: &FreeContext<'_>, simctl: &Simctl) -> Option<Vec<RawRuntime>> {
    ctx.commands
        .run(&simctl.exe, &args(&["runtime", "list", "-j"]), &simctl.env(), TOOL_TIMEOUT)
        .ok()
        .filter(|o| o.success)
        .and_then(|o| simulators::parse_runtimes(&o.stdout))
}

fn args(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| (*s).to_owned()).collect()
}

/// The identifiers of the runtimes the probe would pass now.
pub(super) fn available(ctx: &FreeContext<'_>) -> Vec<String> {
    let Ok(listing) = list(ctx) else { return Vec::new() };
    listing
        .runtimes
        .iter()
        .filter(|r| probe::runtime(&listing.runtimes, listing.devices.as_ref(), &r.identifier, ctx.sources).is_ok())
        .map(|r| r.identifier.clone())
        .collect()
}

pub(super) fn dry_run(ctx: &FreeContext<'_>, identifier: &str) -> ActionReport {
    let listing = match list(ctx) {
        Ok(l) => l,
        Err(reason) => return command(ctx, None, identifier).with(ActionResult::Refused { reason }),
    };
    let mut report = command(ctx, Some(&listing.simctl), identifier);
    let asset = match probe::runtime(&listing.runtimes, listing.devices.as_ref(), identifier, ctx.sources) {
        Ok(asset) => asset,
        Err(reason) => return report.with(ActionResult::Refused { reason }),
    };
    report.bytes_before = measure::allocated(&asset, ctx.skips);
    // The report names the command that would run; the preview is its dry run.
    let mut dry = report.command[1..].to_vec();
    dry.push("--dry-run".to_owned());
    let out = ctx.commands.run(&listing.simctl.exe, &dry, &listing.simctl.env(), TOOL_TIMEOUT);
    match out {
        Ok(o) if o.success => {
            report.preview = Some(Preview {
                lines: o.stdout.lines().map(str::to_owned).collect(),
                estimate_bytes: report.bytes_before,
            });
            report.with(ActionResult::WouldRun)
        }
        Ok(o) => run_failed(report, &crate::exec::tail(&o.stdout, &o.stderr)),
        Err(e) => run_failed(report, &e.to_string()),
    }
}

pub(super) fn run(ctx: &FreeContext<'_>, identifier: &str) -> ActionReport {
    let listing = match list(ctx) {
        Ok(l) => l,
        Err(reason) => return command(ctx, None, identifier).with(ActionResult::Refused { reason }),
    };
    let mut report = command(ctx, Some(&listing.simctl), identifier);
    let asset = match probe::runtime(&listing.runtimes, listing.devices.as_ref(), identifier, ctx.sources) {
        Ok(asset) => asset,
        Err(reason) => return report.with(ActionResult::Refused { reason }),
    };
    report.bytes_before = measure::allocated(&asset, ctx.skips);
    if ctx.cancelled() {
        return report.with(ActionResult::NotRun);
    }
    // The delete and the wait share one deadline: the lock is held for at
    // most one timeout per action.
    let started = Instant::now();
    let exe = listing.simctl.exe.clone();
    let ran = ctx.commands.run(&exe, &report.command[1..], &listing.simctl.env(), ctx.timeout);
    match ran {
        Ok(o) if o.success => report.output = crate::exec::tail(&o.stdout, &o.stderr),
        // Stopped while `simctl` ran: CoreSimulator may have started the
        // delete already, so the list says what happened, not the exit.
        _ if ctx.cancelled() => {
            let state = runtimes(ctx, &listing.simctl)
                .map(|rs| rs.into_iter().find(|r| r.identifier == identifier).map(|r| r.state));
            match state {
                Some(None) => {}
                Some(Some(Some(s))) if s == "Deleting" => return report.with(ActionResult::StillDeleting),
                _ => return stopped(report),
            }
        }
        Ok(o) => return run_failed(report, &crate::exec::tail(&o.stdout, &o.stderr)),
        Err(e) if e.kind() == ErrorKind::TimedOut => {
            return run_failed(report, &format!("timed out after {} s", ctx.timeout.as_secs()));
        }
        Err(e) => return run_failed(report, &e.to_string()),
    }
    // CoreSimulator's daemon deletes it after `simctl` returns.
    let deadline = started + ctx.timeout;
    loop {
        let listed = runtimes(ctx, &listing.simctl).is_none_or(|rs| rs.iter().any(|r| r.identifier == identifier));
        if !listed {
            break;
        }
        if ctx.cancelled() || !wait(ctx, deadline) {
            return report.with(ActionResult::StillDeleting);
        }
    }
    report.bytes_after = measure::allocated(&asset, ctx.skips);
    let shrank = match (report.bytes_before, report.bytes_after) {
        (Some(before), Some(after)) => after < before,
        // Not measured: nothing says it did not shrink.
        _ => true,
    };
    report.with(if shrank { ActionResult::Freed } else { ActionResult::RemovedNotFreed })
}

/// Sleeps one poll interval, in short steps that see a stop. False when the
/// deadline passed or a stop arrived.
fn wait(ctx: &FreeContext<'_>, deadline: Instant) -> bool {
    let until = (Instant::now() + ctx.poll).min(deadline);
    while Instant::now() < until {
        if ctx.cancel.load(Ordering::SeqCst) {
            return false;
        }
        std::thread::sleep((until - Instant::now()).min(Duration::from_millis(50)));
    }
    Instant::now() < deadline && !ctx.cancel.load(Ordering::SeqCst)
}
