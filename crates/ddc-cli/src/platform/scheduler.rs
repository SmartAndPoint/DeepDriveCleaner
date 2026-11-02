//! The scheduled scan, behind one seam per scheduler (story "Scheduled
//! scans on Linux through a systemd user timer", DDC-010): what every
//! backend installs, removes and reports, and the rules they share for the
//! binary they schedule. `cmd/schedule.rs` reaches a backend only through
//! [`Scheduler`]; `platform::scheduler` picks it.

use ddc_core::platform::Canonical as _;
use std::path::{Path, PathBuf};

use crate::cli::IntervalArg;

/// What a scheduled scan runs, whichever scheduler runs it.
#[derive(Debug, Clone)]
pub struct ScheduleSpec {
    pub exe: PathBuf,
    pub interval: IntervalArg,
    /// Written only when given, so `notify_threshold` in settings.json applies otherwise.
    pub threshold: Option<u64>,
    pub log_dir: PathBuf,
}

/// Where an install wrote its schedule, and the command that runs it now.
pub struct Installed {
    pub path: PathBuf,
    pub try_now: String,
}

pub trait Scheduler {
    /// Writes and (re)loads the schedule: idempotent.
    fn install(&self, spec: &ScheduleSpec) -> anyhow::Result<Installed>;
    /// Unloads and removes it: idempotent. Returns whether one was installed.
    fn uninstall(&self) -> anyhow::Result<bool>;
    /// The status lines, `None` when nothing is installed. `current` is the
    /// running binary.
    fn status(&self, current: Option<&Path>) -> Option<Vec<String>>;
}

/// A binary built by cargo (`…/target/<profile>/deepdrivecleaner`) disappears with
/// `cargo clean` or `ddc clean` (rule `rust.target`): refuse to schedule it.
pub fn inside_cargo_target(exe: &Path) -> bool {
    exe.ancestors().any(|a| {
        a.file_name().is_some_and(|n| n == "target") && a.parent().is_some_and(|p| p.join("Cargo.toml").exists())
    })
}

/// Homebrew installs every version under a new directory, which `brew upgrade`
/// removes; a link outside it follows the upgrade. Path arithmetic only:
/// - cask: `<prefix>/Caskroom/<token>/<version>/<file>` → `<prefix>/bin/<file>`
/// - formula: `<prefix>/Cellar/<name>/<version>/bin/<file>` → `<prefix>/opt/<name>/bin/<file>`
pub fn homebrew_stable_path(exe: &Path) -> Option<PathBuf> {
    let file = exe.file_name()?;
    let parent = exe.parent()?;
    let named = |dir: &Path, name: &str| dir.file_name().is_some_and(|n| n == name);
    let caskroom = parent.parent()?.parent()?;
    if named(caskroom, "Caskroom") {
        return Some(caskroom.parent()?.join("bin").join(file));
    }
    let formula = parent.parent()?.parent()?;
    let cellar = formula.parent()?;
    if named(parent, "bin") && named(cellar, "Cellar") {
        return Some(cellar.parent()?.join("opt").join(formula.file_name()?).join("bin").join(file));
    }
    None
}

/// Both paths name the same file once symlinks are resolved.
pub fn same_binary(a: &Path, b: &Path) -> bool {
    matches!((a.canonical(), b.canonical()), (Ok(x), Ok(y)) if x == y)
}

/// The path the schedule runs: Homebrew's stable link when it leads to this binary,
/// so the schedule survives `brew upgrade`; otherwise the binary itself.
pub fn scheduled_program(exe: &Path) -> PathBuf {
    homebrew_stable_path(exe).filter(|link| same_binary(link, exe)).unwrap_or_else(|| exe.to_path_buf())
}

/// How the program an installed schedule runs relates to the running binary.
#[derive(Debug, PartialEq, Eq)]
pub enum ProgramState {
    Missing,
    Other,
    Current,
}

pub fn program_state(program: &Path, current: Option<&Path>) -> ProgramState {
    if !program.exists() {
        ProgramState::Missing
    } else if current.is_some_and(|c| same_binary(program, c)) {
        ProgramState::Current
    } else {
        ProgramState::Other
    }
}
