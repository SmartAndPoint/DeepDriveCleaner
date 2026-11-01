//! What keeps a measured CLI run off the owner's state (story "Memory and
//! CPU under load: peak memory, leaks and CPU time"), decided before every
//! spawn as a pure function of the facts, like the generator's guard
//! ([`crate::target`]). A run gets a fake `HOME`, a data directory inside
//! the layout, no notification, and a generated tree; a clean that trashes
//! goes only to an attached image, whose `.Trashes` leaves with the detach.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use ddc_core::exec::preflight::PreflightSummary;
use ddc_core::platform::procs;

use crate::target::{self, Layout};

/// [`Command::env_clear`] that keeps where an instrumented build writes its
/// coverage profile (`make coverage`, story "Coverage floor and property tests
/// for the safety invariants"). The profile is a file under `target/`, so the
/// run still reads nothing of the owner's; without it the child writes no
/// profile and its code counts as never run. Every test and measured run
/// clears the environment through this (`clippy.toml`).
///
/// It also keeps `SystemRoot`, where Windows itself is installed: Windows
/// libraries a child loads (Winsock, COM, the shell) look up system files
/// through it. It names no user folder; elsewhere it is not set.
pub trait ClearEnv {
    fn clear_env(&mut self) -> &mut Self;

    /// Points every variable a platform reads the home from at `home`
    /// (story "Windows scanner and rule catalog", DDC-007): `HOME` on macOS
    /// and Linux, `USERPROFILE` and the `AppData` folders on Windows. A test
    /// that sets only `HOME` would give a Windows child no home, or, without a
    /// cleared environment, the runner's real profile.
    fn home_env(&mut self, home: impl AsRef<Path>) -> &mut Self;
}

impl ClearEnv for Command {
    #[allow(clippy::disallowed_methods)] // the one clear, the profile's path and the system folder, not the user's environment
    fn clear_env(&mut self) -> &mut Self {
        self.env_clear();
        for keep in KEPT {
            if let Some(value) = std::env::var_os(keep) {
                self.env(keep, value);
            }
        }
        self
    }

    fn home_env(&mut self, home: impl AsRef<Path>) -> &mut Self {
        let home = home.as_ref();
        self.env("HOME", home)
            .env("USERPROFILE", home)
            .env("APPDATA", home.join("AppData").join("Roaming"))
            .env("LOCALAPPDATA", home.join("AppData").join("Local"))
    }
}

const PROFILE_FILE: &str = "LLVM_PROFILE_FILE";
const KEPT: [&str; 2] = [PROFILE_FILE, "SystemRoot"];

/// A file's identity: device and inode.
pub type FileId = (u64, u64);

/// Facts about one run, read by [`facts`].
#[derive(Debug, Clone)]
pub struct Facts<'a> {
    /// The CLI's arguments.
    pub args: &'a [OsString],
    /// `HOME` for the run, and its identity.
    pub home: &'a Path,
    pub home_id: Option<FileId>,
    /// The real home (password database), and its identity.
    pub real_home: &'a Path,
    pub real_home_id: Option<FileId>,
    pub layout: &'a Layout,
    /// The generator's guard on the target, again before every run.
    pub target: Result<(), target::Refusal>,
    /// The target is an attached image the guard accepted.
    pub image: bool,
    /// The run is a clean that moves something to the Trash.
    pub trashes: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    RealHome,
    /// `HOME` is neither in the tree nor the layout's empty home.
    HomeOutside(PathBuf),
    NoDataDir,
    DataDirOutside(PathBuf),
    BackgroundWithoutNoNotify,
    Target(target::Refusal),
    /// A trash on a temporary target would land in the owner's Trash.
    TrashOffImage,
    /// The clean's dry run predicts this many tool runs.
    ToolPredicted(u64),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RealHome => f.write_str("HOME is the real home folder"),
            Self::HomeOutside(p) => write!(f, "HOME {} is outside the generated layout", p.display()),
            Self::NoDataDir => f.write_str("the command line has no --data-dir"),
            Self::DataDirOutside(p) => write!(f, "--data-dir {} is outside the layout's data folder", p.display()),
            Self::BackgroundWithoutNoNotify => f.write_str("--background without --no-notify"),
            Self::Target(r) => write!(f, "the target: {r}"),
            Self::TrashOffImage => f.write_str("a clean that trashes runs only on an attached image"),
            Self::ToolPredicted(n) => write!(f, "the clean's dry run predicts {n} tool run(s)"),
        }
    }
}

impl std::error::Error for Refusal {}

/// The decision; the first unsafe fact refuses the run.
pub fn decide(f: &Facts) -> Result<(), Refusal> {
    let same_id = matches!((f.home_id, f.real_home_id), (Some(a), Some(b)) if a == b);
    if f.home == f.real_home || same_id {
        return Err(Refusal::RealHome);
    }
    if !(inside(f.home, &f.layout.tree) || inside(f.home, &f.layout.home)) {
        return Err(Refusal::HomeOutside(f.home.to_owned()));
    }
    match value_of(f.args, "--data-dir") {
        None => return Err(Refusal::NoDataDir),
        Some(d) if !inside(Path::new(d), &f.layout.data) => {
            return Err(Refusal::DataDirOutside(PathBuf::from(d)));
        }
        Some(_) => {}
    }
    if has(f.args, "--background") && !has(f.args, "--no-notify") {
        return Err(Refusal::BackgroundWithoutNoNotify);
    }
    f.target.clone().map_err(Refusal::Target)?;
    if f.trashes && !f.image {
        return Err(Refusal::TrashOffImage);
    }
    Ok(())
}

/// A clean runs only when its dry run predicts no tool.
pub fn preflight(summary: &PreflightSummary) -> Result<(), Refusal> {
    match summary.would_run {
        0 => Ok(()),
        n => Err(Refusal::ToolPredicted(n)),
    }
}

/// Reads the facts for running `args` with `home` on `layout`, built in
/// `target` (on `device`, for an image).
pub fn facts<'a>(
    args: &'a [OsString],
    home: &'a Path,
    real_home: &'a Path,
    layout: &'a Layout,
    on: &Target,
    trashes: bool,
) -> io::Result<Facts<'a>> {
    let id = |p: &Path| fs::metadata(p).ok().and_then(|m| ddc_core::platform::meta::file_id(&m).ok());
    let target = target::decide(&target::facts(&on.dir, on.device.as_deref(), on.entries)?);
    Ok(Facts {
        args,
        home,
        home_id: id(home),
        real_home,
        real_home_id: id(real_home),
        layout,
        image: on.device.is_some() && target.is_ok(),
        target,
        trashes,
    })
}

/// Where the tree was built.
#[derive(Debug, Clone)]
pub struct Target {
    pub dir: PathBuf,
    pub device: Option<String>,
    pub entries: u64,
}

/// The real home, from the password database.
pub fn real_home() -> io::Result<PathBuf> {
    procs::real_home().ok_or_else(|| io::Error::other("the password database names no home folder"))
}

/// Lexically inside `dir` (or `dir` itself), with no `..` to climb out.
fn inside(path: &Path, dir: &Path) -> bool {
    path.is_absolute() && !path.components().any(|c| c == Component::ParentDir) && path.starts_with(dir)
}

fn has(args: &[OsString], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

/// The value of `--flag value` or `--flag=value`.
fn value_of<'a>(args: &'a [OsString], flag: &str) -> Option<&'a OsStr> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().map(OsString::as_os_str);
        }
        let prefix = format!("{flag}=");
        if let Some(v) = a.to_str().and_then(|s| s.strip_prefix(&prefix)) {
            return Some(OsStr::new(v));
        }
    }
    None
}
