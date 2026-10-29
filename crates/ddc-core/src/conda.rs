//! Conda installs, and what each says its folders are (story "Whole disk:
//! Conda environments and its package cache, with conda"; safety ADR,
//! amendment of 2026-10-29). It reads and parses only: the one command it
//! runs, `conda config --show`, prints conda's computed settings and writes
//! nothing. Conda is never the first `conda` on the tool path: each known
//! install is run as `<prefix>/bin/conda`, through [`CommandRunner::at`].

use std::cell::OnceCell;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde::de::DeserializeOwned;

use crate::exec::CommandRunner;

/// The installers' default prefixes, the same in the home folder and in
/// `/opt`, in the order an install is chosen.
pub const PREFIXES: [&str; 3] = ["miniconda3", "anaconda3", "miniforge3"];

/// Each question's timeout, as a native command's location probe.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// The settings asked of every install.
const SHOW: [&str; 8] =
    ["config", "--show", "root_prefix", "envs_dirs", "pkgs_dirs", "allow_softlinks", "always_softlink", "--json"];

/// The package folders asked right before a clean, with [`PKGS_DIRS_VAR`] set.
const SHOW_PKGS_DIRS: [&str; 4] = ["config", "--show", "pkgs_dirs", "--json"];

/// Limits a clean to one package folder; a comma would split it.
pub const PKGS_DIRS_VAR: &str = "CONDA_PKGS_DIRS";

/// The one clean of a package folder: its packages no environment uses, its
/// tarballs, its index cache and conda's logs. Never `--all`, `--tempfiles`
/// or `--force-pkgs-dirs`: those reach past the folder.
pub const CLEAN: [&str; 6] = ["clean", "--index-cache", "--packages", "--tarballs", "--logfiles", "--yes"];

/// Why a package cache is left alone: no install lists it.
pub const NOT_LISTED: &str = "no conda lists this folder";
/// Why a package cache is left alone: conda would clean another folder too.
pub const NOT_SCOPED: &str = "conda's package folders are set in a .condarc and cannot be limited to this one";
/// Why a package cache is left alone: a package conda calls unused may be
/// linked into an environment.
pub const SOFTLINKS: &str = "conda links packages into environments by symlink";

/// The known installs: the home folder's three, then `/opt`'s three.
pub fn candidates(home: &Path, system_root: &Path) -> Vec<PathBuf> {
    let home = PREFIXES.iter().map(|p| home.join(p));
    home.chain(opt_prefixes(system_root)).collect()
}

/// The all-users installs, by their firmlinked paths (`/opt/miniconda3`).
pub fn opt_prefixes(system_root: &Path) -> Vec<PathBuf> {
    PREFIXES.iter().map(|p| system_root.join("opt").join(p)).collect()
}

/// An install's own `conda`.
pub fn exe(prefix: &Path) -> PathBuf {
    prefix.join("bin/conda")
}

/// The path as conda is given it, in `-p` and in [`PKGS_DIRS_VAR`]: `None`
/// when it is not UTF-8 or holds a comma. Such a path is claimed by no install.
pub fn spelled(path: &Path) -> Option<&str> {
    path.to_str().filter(|s| !s.contains(','))
}

/// A folder holding `conda-meta/history`: an environment, as conda itself tells one.
pub fn is_environment(path: &Path) -> bool {
    std::fs::symlink_metadata(path.join("conda-meta/history")).is_ok_and(|m| m.is_file())
}

/// The environments of an install from the file system, never from conda:
/// every `<prefix>/envs/*` holding `conda-meta/history`, sorted.
pub fn environments(prefix: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = crate::stats::read_dir(prefix.join("envs"))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| is_environment(p))
        .collect();
    out.sort();
    out
}

/// What `conda config --show … --json` prints: conda's computed settings,
/// the defaults when no `.condarc` sets them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Settings {
    pub root_prefix: PathBuf,
    pub envs_dirs: Vec<PathBuf>,
    pub pkgs_dirs: Vec<PathBuf>,
    pub allow_softlinks: bool,
    pub always_softlink: bool,
}

/// The first JSON document of a tool's output; what follows it is ignored.
pub fn first_json<T: DeserializeOwned>(stdout: &str) -> Option<T> {
    serde_json::Deserializer::from_str(stdout).into_iter::<T>().next()?.ok()
}

/// An install that answered.
#[derive(Debug, Clone)]
pub struct Install {
    pub prefix: PathBuf,
    pub exe: PathBuf,
    pub settings: Settings,
}

impl Install {
    /// `path` is this install's base environment.
    pub fn is_root(&self, path: &Path) -> bool {
        same_folder(&self.settings.root_prefix, path)
    }

    /// `env` is one of its environments: its parent is one of `envs_dirs`
    /// (each holds environments, not is one), it holds `conda-meta/history`,
    /// and it is not the base.
    pub fn claims_environment(&self, env: &Path) -> bool {
        let Some(parent) = env.parent().and_then(identity) else { return false };
        self.settings.envs_dirs.iter().filter_map(|d| identity(d)).any(|d| d == parent)
            && is_environment(env)
            && !self.is_root(env)
    }

    /// `folder` is one of its package folders, and not its base.
    pub fn lists_package_folder(&self, folder: &Path) -> bool {
        let Some(id) = identity(folder) else { return false };
        self.settings.pkgs_dirs.iter().filter_map(|d| identity(d)).any(|d| d == id) && !self.is_root(folder)
    }

    /// Environments' files may be symlinks into its package folders.
    pub fn softlinks(&self) -> bool {
        self.settings.allow_softlinks || self.settings.always_softlink
    }

    /// Asked with [`PKGS_DIRS_VAR`] set to `spelled`, the package folders
    /// that exist are exactly `folder`, by `(dev, ino)`. conda merges a
    /// sequence setting from every source, so the variable alone does not
    /// bound a clean; a second existing folder, writable or not, fails it.
    pub fn scoped_to(&self, commands: &dyn CommandRunner, folder: &Path, spelled: &str) -> bool {
        #[derive(Deserialize)]
        struct Answer {
            pkgs_dirs: Vec<PathBuf>,
        }
        let args: Vec<String> = SHOW_PKGS_DIRS.iter().map(|a| (*a).to_owned()).collect();
        let Some(out) =
            commands.run(&self.exe, &args, &[(PKGS_DIRS_VAR, spelled)], PROBE_TIMEOUT).ok().filter(|o| o.success)
        else {
            return false;
        };
        let Some(answer) = first_json::<Answer>(&out.stdout) else { return false };
        let mut ids: Vec<(u64, u64)> = answer.pkgs_dirs.iter().filter_map(|d| identity(d)).collect();
        ids.sort_unstable();
        ids.dedup();
        ids.len() == 1 && identity(folder) == ids.first().copied()
    }
}

/// Asks the install at `prefix`; `None` when it has no `bin/conda`, or its
/// answer is a failure, a timeout or JSON ddc cannot read: it claims nothing.
pub fn ask(commands: &dyn CommandRunner, prefix: &Path) -> Option<Install> {
    let exe = commands.at(&exe(prefix))?;
    let args: Vec<String> = SHOW.iter().map(|a| (*a).to_owned()).collect();
    let out = commands.run(&exe, &args, &[], PROBE_TIMEOUT).ok().filter(|o| o.success)?;
    let settings = first_json(&out.stdout)?;
    Some(Install { prefix: prefix.to_path_buf(), exe, settings })
}

/// The answers of one clean, or of one area-action run: every candidate
/// that exists is asked once, the first time any is needed.
pub struct Installs<'a> {
    commands: &'a dyn CommandRunner,
    candidates: Vec<PathBuf>,
    answers: OnceCell<Vec<Install>>,
}

impl<'a> Installs<'a> {
    pub fn new(commands: &'a dyn CommandRunner, candidates: Vec<PathBuf>) -> Self {
        Self { commands, candidates, answers: OnceCell::new() }
    }

    /// The installs that answered, in the order of the candidates.
    pub fn get(&self) -> &[Install] {
        self.answers.get_or_init(|| self.candidates.iter().filter_map(|p| ask(self.commands, p)).collect())
    }
}

/// Which install, if any, removes an environment.
#[derive(Debug)]
pub enum EnvironmentClaim<'i> {
    /// The first install that claims it.
    Install(&'i Install),
    /// Some install's base environment: never removed.
    Base,
    Unclaimed,
}

/// The first install claiming `env`; the base of any install is refused
/// first, whichever install lists its parent.
pub fn environment<'i>(installs: &'i [Install], env: &Path) -> EnvironmentClaim<'i> {
    if installs.iter().any(|i| i.is_root(env)) {
        return EnvironmentClaim::Base;
    }
    installs.iter().find(|i| i.claims_environment(env)).map_or(EnvironmentClaim::Unclaimed, EnvironmentClaim::Install)
}

/// The first install listing the package folder `folder`, unless one that
/// lists it links packages by symlink. The error is the reason, in words.
pub fn package_folder<'i>(installs: &'i [Install], folder: &Path) -> Result<&'i Install, &'static str> {
    let mut listing = installs.iter().filter(|i| i.lists_package_folder(folder)).peekable();
    let first = *listing.peek().ok_or(NOT_LISTED)?;
    if listing.any(Install::softlinks) {
        return Err(SOFTLINKS);
    }
    Ok(first)
}

/// `(dev, ino)` of what `path` names, following symlinks; `None` when it
/// does not exist.
pub fn identity(path: &Path) -> Option<(u64, u64)> {
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

/// Both are folders and the same one, by `(dev, ino)`: `/opt` and
/// `/System/Volumes/Data/opt` spell one folder through a firmlink.
pub fn same_folder(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.is_dir() && b.is_dir() && a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_comma_or_a_non_utf8_path_is_never_given_to_conda() {
        use std::os::unix::ffi::OsStrExt as _;
        assert_eq!(spelled(Path::new("/Users/example/.conda/pkgs")), Some("/Users/example/.conda/pkgs"));
        assert_eq!(spelled(Path::new("/Users/example/a,b/pkgs")), None);
        assert_eq!(spelled(Path::new(std::ffi::OsStr::from_bytes(b"/Users/example/\xff"))), None);
    }

    #[test]
    fn only_the_first_json_document_is_read() {
        let s: Option<Settings> = first_json(
            r#"{"root_prefix": "/opt/miniconda3", "envs_dirs": [], "pkgs_dirs": [], "allow_softlinks": false, "always_softlink": false}
DryRunExit: Dry run. Exiting."#,
        );
        assert_eq!(s.unwrap().root_prefix, PathBuf::from("/opt/miniconda3"));
        assert!(first_json::<Settings>("not json").is_none());
        assert!(first_json::<Settings>(r#"{"root_prefix": "/x"}"#).is_none());
    }
}
