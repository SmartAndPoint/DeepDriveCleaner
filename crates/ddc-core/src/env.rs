//! Process environment. This is the only place that reads `$HOME` (through
//! `platform::folders`), `DDC_DATA_DIR`, `DOCKER_CONFIG`, `CLAUDE_CODE_TMPDIR`
//! and `TMPDIR`; everything else receives paths through [`Env`], so tests
//! can point the whole tool at temporary directories.

use crate::platform::Canonical as _;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::platform::Platform;

#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    #[error("HOME is not set")]
    NoHome,
    #[error("cannot resolve home directory {path}: {source}")]
    Home {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Clone)]
pub struct Env {
    home: PathBuf,
    data_dir: PathBuf,
    log_dir: PathBuf,
    now: SystemTime,
    /// Directories of `PATH`, where tools for native cleanup commands are looked up.
    path: Vec<PathBuf>,
    /// `DOCKER_CONFIG`, where docker keeps its contexts; unset in tests.
    docker_config: Option<PathBuf>,
    /// Where `/opt` and the other system folders are looked up: `/`, or a
    /// fixture root in tests.
    system_root: PathBuf,
    /// `CLAUDE_CODE_TMPDIR`, where Claude Code puts its temporary folder
    /// instead of `/tmp`; unset in tests and under a fixture system root.
    claude_tmpdir: Option<PathBuf>,
    /// `TMPDIR`, Linux's temporary folder; unset in tests and under a fixture
    /// system root.
    tmpdir: Option<PathBuf>,
    /// Neither `--data-dir` nor `DDC_DATA_DIR` was given: the data directory
    /// is the app's, so a scan saved here is the one the app shows.
    default_data_dir: bool,
}

impl Env {
    /// Environment variable that overrides the data directory.
    pub const DATA_DIR_VAR: &'static str = "DDC_DATA_DIR";

    /// Builds the environment from the running process. `data_dir` (from
    /// `--data-dir`) wins over `DDC_DATA_DIR`; when either is given, logs go
    /// to `<data_dir>/logs` so nothing lands in the real `~/Library`.
    #[allow(clippy::disallowed_methods)] // the one sanctioned reader of the process environment
    pub fn from_process(data_dir: Option<PathBuf>) -> Result<Self, EnvError> {
        let home = crate::platform::folders::home().ok_or(EnvError::NoHome)?;
        let home = home.canonical().map_err(|source| EnvError::Home { path: home.clone(), source })?;
        let data_dir =
            data_dir.or_else(|| std::env::var_os(Self::DATA_DIR_VAR).filter(|v| !v.is_empty()).map(PathBuf::from));
        let default_data_dir = data_dir.is_none();
        let (data_dir, log_dir) = match data_dir {
            Some(dir) => (dir.clone(), dir.join("logs")),
            None => crate::platform::folders::default_data_and_log(&home),
        };
        let path = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        let docker_config = std::env::var_os("DOCKER_CONFIG").filter(|v| !v.is_empty()).map(PathBuf::from);
        let claude_tmpdir = std::env::var_os("CLAUDE_CODE_TMPDIR").filter(|v| !v.is_empty()).map(PathBuf::from);
        let tmpdir = std::env::var_os("TMPDIR").filter(|v| !v.is_empty()).map(PathBuf::from);
        Ok(Self {
            home,
            data_dir,
            log_dir,
            now: SystemTime::now(),
            path,
            docker_config,
            system_root: PathBuf::from("/"),
            claude_tmpdir,
            tmpdir,
            default_data_dir,
        })
    }

    /// Explicit environment for tests and embedding; `home` is canonicalized.
    pub fn with_paths(home: &Path, data_dir: &Path, log_dir: &Path, now: SystemTime) -> io::Result<Self> {
        Ok(Self {
            home: home.canonical()?,
            data_dir: data_dir.to_path_buf(),
            log_dir: log_dir.to_path_buf(),
            now,
            path: Vec::new(),
            docker_config: None,
            system_root: PathBuf::from("/"),
            claude_tmpdir: None,
            tmpdir: None,
            default_data_dir: false,
        })
    }

    /// Replaces the system root (tests put `/opt` under a fixture root). The
    /// temporary-folder variables are dropped with it: under a fixture root
    /// nothing outside that root is looked at.
    pub fn with_system_root(mut self, root: &Path) -> Self {
        self.system_root = root.to_path_buf();
        self.claude_tmpdir = None;
        self.tmpdir = None;
        self
    }

    /// Replaces the tool search path (tests point it at fake tools).
    pub fn with_search_path(mut self, path: Vec<PathBuf>) -> Self {
        self.path = path;
        self
    }

    /// `PATH` plus the usual tool locations that GUI and launchd processes
    /// do not have in their `PATH`.
    pub fn tool_dirs(&self) -> Vec<PathBuf> {
        // Only absolute entries: an empty or `.` PATH entry would run tools from the cwd.
        let mut dirs: Vec<PathBuf> = self.path.iter().filter(|d| d.is_absolute()).cloned().collect();
        dirs.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
        dirs.extend([".cargo/bin", ".local/bin", "go/bin"].map(|rel| self.home.join(rel)));
        dirs.dedup();
        dirs
    }

    /// `NO_COLOR` is set (<https://no-color.org>).
    #[allow(clippy::disallowed_methods)] // process environment, read here only
    pub fn no_color() -> bool {
        std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// `/`, where the system's own folders are; a fixture root in tests.
    pub fn system_root(&self) -> &Path {
        &self.system_root
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Whether the data directory is the default one, the app's: neither
    /// `--data-dir` nor `DDC_DATA_DIR` was given.
    pub fn data_dir_is_default(&self) -> bool {
        self.default_data_dir
    }

    pub fn log_dir(&self) -> &Path {
        &self.log_dir
    }

    pub fn now(&self) -> SystemTime {
        self.now
    }

    /// docker's config folder: `DOCKER_CONFIG`, else `~/.docker`.
    pub fn docker_config(&self) -> PathBuf {
        self.docker_config.clone().unwrap_or_else(|| self.home.join(".docker"))
    }

    /// Claude Code's temporary folder for the current user, whose
    /// `<project-slug>/<session>/` folders hold each session's scratchpad,
    /// tasks and images; `None` where ddc knows no place for it. Not
    /// canonical, and it may not exist.
    pub fn claude_temp_base(&self) -> Option<PathBuf> {
        claude_temp_base(
            crate::platform::PLATFORM,
            &self.system_root,
            self.claude_tmpdir.as_deref(),
            self.tmpdir.as_deref(),
            crate::platform::procs::uid(),
        )
    }
}

/// Where Claude Code keeps its temporary folder: `claude-<uid>` under
/// `CLAUDE_CODE_TMPDIR` when it is set, else under `/private/tmp` on macOS
/// (Claude Code uses `/tmp`, not the per-user `TMPDIR`, there), else under
/// `TMPDIR` or `/tmp` on Linux. The system folders are looked up under
/// `system_root`.
///
/// Assumed, not yet confirmed by the research (story "Per-session
/// footprint: what each agent session keeps in its transcripts, temporary
/// folders and worktrees"): Claude Code appends `claude-<uid>` under
/// `CLAUDE_CODE_TMPDIR` as it does under `/tmp`.
pub(crate) fn claude_temp_base(
    platform: Platform,
    system_root: &Path,
    claude_tmpdir: Option<&Path>,
    tmpdir: Option<&Path>,
    uid: u32,
) -> Option<PathBuf> {
    let parent = match (claude_tmpdir, platform) {
        (Some(dir), Platform::Macos | Platform::Linux) => dir.to_path_buf(),
        (None, Platform::Macos) => system_root.join("private/tmp"),
        (None, Platform::Linux) => tmpdir.map_or_else(|| system_root.join("tmp"), Path::to_path_buf),
        (_, Platform::Windows) => return None,
    };
    Some(parent.join(format!("claude-{uid}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_temp_base_follows_the_platform_and_the_override() {
        let root = Path::new("/fixture");
        let over = Path::new("/scratch");
        let tmp = Path::new("/var/tmp/me");
        let base = |platform, claude, tmpdir| claude_temp_base(platform, root, claude, tmpdir, 501);
        let at = |p: &str| Some(PathBuf::from(p));
        assert_eq!(base(Platform::Macos, None, Some(tmp)), at("/fixture/private/tmp/claude-501"), "TMPDIR is not it");
        assert_eq!(base(Platform::Macos, Some(over), None), at("/scratch/claude-501"));
        assert_eq!(base(Platform::Linux, None, None), at("/fixture/tmp/claude-501"));
        assert_eq!(base(Platform::Linux, None, Some(tmp)), at("/var/tmp/me/claude-501"));
        assert_eq!(base(Platform::Linux, Some(over), Some(tmp)), at("/scratch/claude-501"));
        assert_eq!(base(Platform::Windows, Some(over), Some(tmp)), None);
    }

    #[test]
    fn a_fixture_system_root_drops_the_temporary_variables() {
        let env = Env {
            home: PathBuf::from("/home"),
            data_dir: PathBuf::from("/data"),
            log_dir: PathBuf::from("/data/logs"),
            now: SystemTime::UNIX_EPOCH,
            path: Vec::new(),
            docker_config: None,
            system_root: PathBuf::from("/"),
            claude_tmpdir: Some(PathBuf::from("/scratch")),
            tmpdir: Some(PathBuf::from("/var/tmp/me")),
            default_data_dir: false,
        }
        .with_system_root(Path::new("/fixture"));
        let base = env.claude_temp_base();
        if crate::platform::PLATFORM != Platform::Windows {
            assert!(base.unwrap().starts_with("/fixture"));
        }
    }
}
