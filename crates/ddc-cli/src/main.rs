//! `ddc` — DeepDriveCleaner command-line interface.

mod cli;
mod cmd;
mod disclaimer;
mod exit;
mod gate;
mod notify;
mod platform;
mod progress;
mod prompt;
mod render;
mod size;

use std::process::ExitCode;

use clap::Parser as _;

fn main() -> ExitCode {
    // A no-op unless built with `ddc-core/dhat-heap` (`make heap-profile`).
    let _heap = ddc_core::heap::start();
    // `try_parse`, so help and usage errors get the warning too; `--version` does not.
    let parsed = cli::Cli::try_parse();
    if !matches!(&parsed, Err(e) if e.kind() == clap::error::ErrorKind::DisplayVersion) {
        disclaimer::warn_if_terminal();
    }
    let cli = parsed.unwrap_or_else(|e| e.exit());
    // Before the process setup, which is OS code too, and before any `Env`,
    // lock or signal handler.
    if let Some(code) = gate::check(&cli.command) {
        return code;
    }
    let json = cli.command.prints_json();
    platform::init_process();
    match cmd::run(cli) {
        Ok(code) => code,
        Err(err) => {
            // A core refusal the gate missed exits the same way.
            if let Some(refusal) = ddc_core::platform::refusal_in(err.as_ref()) {
                return gate::refuse(&refusal, &format!("{err:#}"), json);
            }
            eprintln!("ddc: {err:#}");
            let busy = err
                .downcast_ref::<ddc_core::store::LockError>()
                .is_some_and(|e| matches!(e, ddc_core::store::LockError::Busy(_)));
            if busy { ExitCode::from(exit::BUSY) } else { ExitCode::FAILURE }
        }
    }
}
