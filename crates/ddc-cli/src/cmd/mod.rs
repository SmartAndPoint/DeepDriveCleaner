//! Subcommand dispatch.

mod background;
mod clean;
mod diag;
mod du;
mod scan;
mod schedule;

use std::process::ExitCode;

use crate::cli::{Cli, Command};

pub fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Command::Scan(args) => scan::run(args),
        Command::Clean(args) => clean::run(args),
        Command::Schedule { action } => schedule::run(action),
        Command::Du(args) => du::run(args),
        Command::Diag => diag::run(),
    }
}
