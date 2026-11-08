//! Subcommand dispatch.

mod agent_footprint;
mod agent_status;
mod background;
mod changes;
mod clean;
mod clean_items;
mod diag;
mod disk;
mod disk_brew_packages;
mod disk_docker;
mod disk_free;
mod du;
mod keep;
mod lock;
mod notify;
mod rules;
mod scan;
mod schedule;
mod space;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ddc_core::env::Env;
use ddc_core::model::ScanReport;
use ddc_core::report::recheck::{same_roots, walked_roots};
use ddc_core::settings::{self, Resolved, ScanFlags, Settings};
use ddc_core::store::{Kept, PREVIOUS_SCAN};

use crate::cli::{AgentAction, Cli, Command, DiskAction, DiskArgs, KeepAction, LockAction};

pub fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    match cli.command {
        Command::Scan(args) => scan::run(args),
        Command::Clean(args) => clean::run(args),
        Command::Rules(args) => rules::run(&args),
        Command::Disk(DiskArgs { action: Some(DiskAction::Agents(args)), .. }) => agent_status::run(&args),
        Command::Disk(DiskArgs { action: Some(DiskAction::Free(args)), .. }) => disk_free::run(&args),
        Command::Disk(DiskArgs { action: Some(DiskAction::BrewPackages(args)), .. }) => disk_brew_packages::run(&args),
        Command::Disk(DiskArgs { action: Some(DiskAction::Docker(args)), .. }) => disk_docker::run(&args),
        Command::Disk(args) => disk::run(args),
        Command::Space(args) => space::run(&args),
        Command::Changes(args) => changes::run(&args),
        Command::Agent { action: AgentAction::Status(args) } => agent_status::run(&args),
        Command::Agent { action: AgentAction::Footprint(args) } => agent_footprint::run(&args),
        Command::Keep { action: KeepAction::Remove(args) } => keep::remove(&args),
        Command::Lock { action: LockAction::Status(args) } => lock::status(&args),
        Command::Lock { action: LockAction::Stop(args) } => lock::stop(&args),
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

/// Whether a scan given paths (`given`) walked other roots than a scan without
/// paths does, as a clean judges it (`same_roots`). Then its report in the
/// app's data directory replaces the app's own until its next scan (story
/// "Review window: say when the saved scan is not of the home folder"), and
/// the `ddc clean` it suggests names its roots. No Settings root existing
/// counts as other roots.
fn own_roots(given: bool, report: &ScanReport, env: &Env, settings: &Settings) -> bool {
    given && !settings.resolve(env, &ScanFlags::default()).is_ok_and(|r| same_roots(report, env, &r.options))
}

/// The roots a scan without paths walks, as its report names them: a save
/// keeps the report it replaces only when it is of these
/// (`store::write_last_scan`). `None` when no Settings root resolves.
fn default_roots(env: &Env, settings: &Settings) -> Option<Vec<PathBuf>> {
    settings.resolve(env, &ScanFlags::default()).ok().and_then(|r| walked_roots(env, &r.options))
}

/// What to say when a save could not keep the report it replaced; the save
/// itself went on.
fn not_kept(env: &Env, kept: &Kept) -> Option<String> {
    match kept {
        Kept::Failed(e) => {
            Some(format!("cannot keep the previous report in {}: {e}", env.data_dir().join(PREVIOUS_SCAN).display()))
        }
        Kept::Kept { .. } | Kept::Nothing => None,
    }
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
