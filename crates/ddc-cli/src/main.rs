//! `ddc` — DeepDriveCleaner command-line interface.

mod cli;
mod cmd;
mod disclaimer;
mod exit;
mod launchd;
mod notify;
mod progress;
mod prompt;
mod render;
mod size;

use std::process::ExitCode;

use clap::Parser as _;

fn main() -> ExitCode {
    // `try_parse`, so help and usage errors get the warning too; `--version` does not.
    let parsed = cli::Cli::try_parse();
    if !matches!(&parsed, Err(e) if e.kind() == clap::error::ErrorKind::DisplayVersion) {
        disclaimer::warn_if_terminal();
    }
    let cli = parsed.unwrap_or_else(|e| e.exit());
    init_process();
    match cmd::run(cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("ddc: {err:#}");
            let busy = err
                .downcast_ref::<ddc_core::store::LockError>()
                .is_some_and(|e| matches!(e, ddc_core::store::LockError::Busy(_)));
            if busy { ExitCode::from(exit::BUSY) } else { ExitCode::FAILURE }
        }
    }
}

/// Process-wide setup that must happen before any filesystem access.
fn init_process() {
    // Never download evicted iCloud / File Provider content while scanning.
    if let Err(err) = ddc_core::sys::iopolicy::disable_dataless_materialization() {
        eprintln!("ddc: warning: cannot disable dataless file materialization: {err}");
    }
    // launchd starts agents with 256 open files; the walker needs headroom.
    if let Err(err) = ddc_core::sys::rlimit::raise_nofile_limit() {
        eprintln!("ddc: warning: cannot raise the open-file limit: {err}");
    }
}
