//! `ddc lock status`: who holds the data-dir lock now, and `ddc lock stop`,
//! which interrupts one of them (story "Busy: name the scan or clean that
//! holds the lock, and offer to stop it"; ADR "Menu bar app: Swift shell over
//! the ddc CLI", §6 item 16). Both read the holders' records only: they
//! never probe the lock, take none and remove nothing. `stop` sends one
//! SIGINT to one process, and only to a live holder that runs ddc's binary;
//! never SIGKILL, never a process group.

use std::io::{self, Write};
use std::process::ExitCode;
use std::time::SystemTime;

use ddc_core::env::Env;
use ddc_core::holders::{self, HolderCommand, HolderPhase, LOCK_SCHEMA_VERSION, LockStatus, StopRefusal};
use ddc_core::platform::{procs, signals};

use crate::cli::{LockStatusArgs, LockStopArgs};
use crate::{exit, render};

pub fn status(args: &LockStatusArgs) -> anyhow::Result<ExitCode> {
    let env = Env::from_process(args.data_dir.clone())?;
    let report = LockStatus { schema_version: LOCK_SCHEMA_VERSION, holders: holders::holders(&env) };
    let mut out = io::stdout().lock();
    let written = if args.json {
        serde_json::to_writer_pretty(&mut out, &report).map_err(io::Error::from).and_then(|()| writeln!(out))
    } else if report.holders.is_empty() {
        writeln!(out, "No scan or clean holds the lock.")
    } else {
        let now = humantime::format_rfc3339_seconds(SystemTime::now()).to_string();
        report.holders.iter().try_for_each(|h| writeln!(out, "{}", render::holder(h, &now)))
    };
    match written {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(ExitCode::SUCCESS),
        other => other.map(|()| ExitCode::SUCCESS).map_err(Into::into),
    }
}

pub fn stop(args: &LockStopArgs) -> anyhow::Result<ExitCode> {
    if procs::is_root() {
        eprintln!("ddc lock stop: refusing to run as root; run it as your user");
        return Ok(ExitCode::from(exit::REFUSED));
    }
    let env = Env::from_process(args.data_dir.clone())?;
    let live = holders::holders(&env);
    let exe = procs::executable(args.pid);
    let holder = match holders::stoppable(&live, args.pid, args.started, exe.as_deref()) {
        Ok(holder) => holder,
        Err(StopRefusal::NoLiveRecord) => {
            eprintln!(
                "ddc lock stop: no scan or clean that holds the lock runs as pid {} started at {}; nothing was sent",
                args.pid, args.started
            );
            return Ok(ExitCode::from(exit::REFUSED));
        }
        Err(StopRefusal::NotDdc) => {
            let what =
                exe.map_or_else(|| "its executable cannot be read".to_owned(), |e| format!("it runs {}", e.display()));
            eprintln!("ddc lock stop: pid {} is not ddc's binary ({what}); nothing was sent", args.pid);
            return Ok(ExitCode::from(exit::REFUSED));
        }
    };
    // The narrowest window we can give pid reuse: the start is read again
    // right before the signal (no pidfd on macOS; ADR §6 item 16).
    if !holders::live(holder.start_us, procs::process_start(holder.pid), procs::uid()) {
        return Ok(ended(holder.pid));
    }
    if let Err(e) = signals::interrupt(holder.pid) {
        if procs::process_start(holder.pid).is_none() {
            return Ok(ended(holder.pid));
        }
        if e.kind() == std::io::ErrorKind::InvalidInput {
            eprintln!("ddc lock stop: pid {} cannot be interrupted ({e}); nothing was sent", holder.pid);
            return Ok(ExitCode::from(exit::REFUSED));
        }
        return Err(anyhow::Error::from(e).context(format!("cannot interrupt pid {}", holder.pid)));
    }
    let after = match (holder.command, holder.phase) {
        (_, Some(HolderPhase::Cleaning)) => "it stops after its current item",
        (_, Some(HolderPhase::Preparing)) => "it ends before its first item, with nothing removed",
        (HolderCommand::DiskFree, _) => "it stops before its next action; a tool's command is interrupted",
        (HolderCommand::DiskFreeDryRun, _) => "it ends after its current check, with nothing run",
        _ => "it ends at once",
    };
    let now = humantime::format_rfc3339_seconds(SystemTime::now()).to_string();
    println!("Interrupted {}: {after}.", render::holder(holder, &now));
    Ok(ExitCode::SUCCESS)
}

/// The holder ended between the check and the signal: nothing was sent.
fn ended(pid: i32) -> ExitCode {
    eprintln!("ddc lock stop: pid {pid} has ended; nothing was sent");
    ExitCode::from(exit::REFUSED)
}
