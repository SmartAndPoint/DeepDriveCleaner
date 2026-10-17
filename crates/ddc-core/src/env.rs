//! Process environment. This is the only place that reads `$HOME` and
//! `DDC_DATA_DIR`; everything else receives paths through [`Env`], so tests
//! can point the whole tool at temporary directories.

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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
}

impl Env {
    /// Environment variable that overrides the data directory.
    pub const DATA_DIR_VAR: &'static str = "DDC_DATA_DIR";

    /// Builds the environment from the running process. `data_dir` (from
    /// `--data-dir`) wins over `DDC_DATA_DIR`; when either is given, logs go
    /// to `<data_dir>/logs` so nothing lands in the real `~/Library`.
    #[allow(clippy::disallowed_methods)] // the one sanctioned reader of the process environment
    pub fn from_process(data_dir: Option<PathBuf>) -> Result<Self, EnvError> {
        let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from).ok_or(EnvError::NoHome)?;
        let home = home.canonicalize().map_err(|source| EnvError::Home { path: home.clone(), source })?;
        let data_dir =
            data_dir.or_else(|| std::env::var_os(Self::DATA_DIR_VAR).filter(|v| !v.is_empty()).map(PathBuf::from));
        let (data_dir, log_dir) = match data_dir {
            Some(dir) => (dir.clone(), dir.join("logs")),
            None => {
                (home.join("Library/Application Support/DeepDriveCleaner"), home.join("Library/Logs/DeepDriveCleaner"))
            }
        };
        let path = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
        Ok(Self { home, data_dir, log_dir, now: SystemTime::now(), path })
    }

    /// Explicit environment for tests and embedding; `home` is canonicalized.
    pub fn with_paths(home: &Path, data_dir: &Path, log_dir: &Path, now: SystemTime) -> io::Result<Self> {
        Ok(Self {
            home: home.canonicalize()?,
            data_dir: data_dir.to_path_buf(),
            log_dir: log_dir.to_path_buf(),
            now,
            path: Vec::new(),
        })
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

    /// `NO_COLOR` is set (https://no-color.org).
    #[allow(clippy::disallowed_methods)] // process environment, read here only
    pub fn no_color() -> bool {
        std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn log_dir(&self) -> &Path {
        &self.log_dir
    }

    pub fn now(&self) -> SystemTime {
        self.now
    }
}
