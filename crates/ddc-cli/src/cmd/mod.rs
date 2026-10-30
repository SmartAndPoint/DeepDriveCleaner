//! Subcommand dispatch.

mod background;
mod clean;
mod clean_items;
mod diag;
mod disk;
mod disk_agents;
mod disk_brew_packages;
mod disk_free;
mod du;
mod keep;
mod notify;
mod rules;
mod scan;
mod schedule;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ddc_core::env::Env;
use ddc_core::settings::{self, Resolved, ScanFlags};

use crate::cli::{Cli, Command, DiskAction, DiskArgs, KeepAction};

pub fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Command::Scan(args) => scan::run(args),
        Command::Clean(args) => clean::run(args),
        Command::Rules(args) => rules::run(&args),
        Command::Disk(DiskArgs { action: Some(DiskAction::Agents(args)), .. }) => disk_agents::run(&args),
        Command::Disk(DiskArgs { action: Some(DiskAction::Free(args)), .. }) => disk_free::run(&args),
        Command::Disk(DiskArgs { action: Some(DiskAction::BrewPackages(args)), .. }) => disk_brew_packages::run(&args),
        Command::Disk(args) => disk::run(args),
        Command::Keep { action: KeepAction::Remove(args) } => keep::remove(&args),
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

/// A list the app passes: the file, or stdin for `-`.
fn read_list(path: &Path) -> std::io::Result<Vec<u8>> {
    if path.as_os_str() == "-" {
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut std::io::stdin().lock(), &mut bytes)?;
        Ok(bytes)
    } else {
        std::fs::read(path)
    }
}
