//! What Homebrew says is installed, read only (story "Whole disk: Homebrew
//! packages, their sizes, and brew uninstall"; safety ADR, amendment of
//! 2026-10-29). The two commands it runs, `brew info --json=v2 --installed`
//! and `brew services list --json`, print and write nothing of their own;
//! the package listing and the re-read before a removal both go through
//! here. Every name is matched as brew's own code matches it: a dependency
//! by the part of its name after the last `/`.

pub mod graph;
pub mod info;
pub mod listing;
mod model;
pub mod services;

use std::path::Path;
use std::time::{Duration, Instant};

pub use graph::{Graph, NeededBy, Node};
pub use info::Installed;
pub use model::*;
pub use services::{Service, Services};

use crate::exec::CommandRunner;

/// Every installed formula and cask, as JSON.
pub const INFO: [&str; 3] = ["info", "--json=v2", "--installed"];

/// The service jobs brew knows of, as JSON.
pub const SERVICES_LIST: [&str; 3] = ["services", "list", "--json"];

/// The environment of `info`, `services list`, the removal and its location
/// probe: `brew cleanup`'s three (`free::brew::BREW_ENV`), and no refresh of
/// Homebrew's API files when they are only stale.
pub const ENV: [(&str, &str); 4] = [
    ("HOMEBREW_NO_AUTOREMOVE", "1"),
    ("HOMEBREW_NO_ENV_HINTS", "1"),
    ("NO_COLOR", "1"),
    ("HOMEBREW_NO_AUTO_UPDATE", "1"),
];

/// A formula's name as a Cellar rack holds it: `user/tap/foo` is `foo`.
pub fn short(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// Whether `brew services` can be asked without fetching anything: it is
/// part of brew itself (since March 2025), or the old tap is there. With
/// neither, the command would clone the tap.
pub fn has_services(prefix: &Path) -> bool {
    prefix.join("Library/Homebrew/services").exists() || prefix.join("Library/Taps/homebrew/homebrew-services").exists()
}

/// One time limit shared by every brew command of a step (menu bar ADR §6,
/// amendment of 2026-10-29): the probe's three queries, the re-read and
/// the removal together, so the lock is held for at most one limit.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    start: Instant,
    total: Duration,
}

impl Budget {
    /// Starts now.
    pub fn new(total: Duration) -> Self {
        Self { start: Instant::now(), total }
    }

    pub fn total(&self) -> Duration {
        self.total
    }

    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }

    /// What is left, zero once it is spent.
    pub fn remaining(&self) -> Duration {
        self.total.saturating_sub(self.start.elapsed())
    }
}

/// Why brew's answer cannot be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskError {
    /// It exited non-zero or printed what is not JSON: its output's tail.
    Failed(String),
    /// It did not finish within the time it was given.
    TimedOut,
}

/// `brew info --json=v2 --installed` under [`ENV`].
pub fn ask_info(commands: &dyn CommandRunner, exe: &Path, timeout: Duration) -> Result<Installed, AskError> {
    let out = run(commands, exe, &INFO, timeout)?;
    info::parse(&out).ok_or_else(|| AskError::Failed("brew printed what is not its package list".to_owned()))
}

/// `brew services list --json` under [`ENV`], when brew has it.
pub fn ask_services(commands: &dyn CommandRunner, exe: &Path, prefix: &Path, timeout: Duration) -> Services {
    if !has_services(prefix) {
        return Services::NotKnown;
    }
    match run(commands, exe, &SERVICES_LIST, timeout) {
        Ok(out) => services::parse(&out).map_or(Services::Unreadable, Services::Listed),
        Err(_) => Services::Unreadable,
    }
}

fn run(commands: &dyn CommandRunner, exe: &Path, args: &[&str], timeout: Duration) -> Result<String, AskError> {
    let args: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
    match commands.run(exe, &args, &ENV, timeout) {
        Ok(o) if o.success => Ok(o.stdout),
        Ok(o) => Err(AskError::Failed(crate::exec::tail(&o.stdout, &o.stderr))),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Err(AskError::TimedOut),
        Err(e) => Err(AskError::Failed(e.to_string())),
    }
}
