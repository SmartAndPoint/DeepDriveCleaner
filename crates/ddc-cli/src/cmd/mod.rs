//! Subcommand dispatch.

mod background;
mod clean;
mod clean_items;
mod diag;
mod du;
mod notify;
mod rules;
mod scan;
mod schedule;

use std::path::PathBuf;
use std::process::ExitCode;

use ddc_core::env::Env;
use ddc_core::settings::{self, Resolved, ScanFlags};

use crate::cli::{Cli, Command};

pub fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Command::Scan(args) => scan::run(args),
        Command::Clean(args) => clean::run(args),
        Command::Rules(args) => rules::run(&args),
        Command::Notify { action } => notify::run(action),
        Command::Schedule { action } => schedule::run(action),
        Command::Du(args) => du::run(args),
        Command::Diag => diag::run(),
    }
}

/// A command's scan options: `settings.json` under its flags.
fn resolve(env: &Env, flags: ScanFlags) -> anyhow::Result<Resolved> {
    Ok(settings::load(env)?.resolve(env, &flags)?)
}

/// Settings folders that do not exist (an unplugged disk) were skipped.
fn note_missing(missing: &[PathBuf]) {
    for path in missing {
        eprintln!("ddc: {} in settings.json does not exist; skipped", path.display());
    }
}
