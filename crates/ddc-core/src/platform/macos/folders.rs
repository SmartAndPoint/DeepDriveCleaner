//! The user's home and ddc's own folders under it, on macOS. The one item of
//! this module that reads the environment, and only `HOME` (ADR "Platform
//! seam: one platform module per OS with declared capabilities", §8).

use std::path::{Path, PathBuf};

/// `$HOME`, when set and not empty. `Env` canonicalizes it.
#[allow(clippy::disallowed_methods)] // the platform's reader of HOME; `Env::from_process` is its one caller
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

/// The data and log folders when neither `--data-dir` nor `DDC_DATA_DIR`
/// names one: where the app reads its scans.
pub fn default_data_and_log(home: &Path) -> (PathBuf, PathBuf) {
    (home.join("Library/Application Support/DeepDriveCleaner"), home.join("Library/Logs/DeepDriveCleaner"))
}

/// The folders the environment redirects that are protected wherever they
/// point: none here (safety ADR §6, Windows, redirected folders).
pub fn redirected() -> Vec<PathBuf> {
    Vec::new()
}

/// The program of a git probe (story "Worktree findings and provenance",
/// decision 10).
pub const GIT_PROGRAM: &str = "git";
/// Folders whose git is never run: `/usr/bin/git` is the `xcrun` shim that offers to install the developer tools.
pub const GIT_SHIM_DIRS: &[&str] = &["/usr/bin"];
/// Searched for git after the tool folders.
pub const GIT_SYSTEM_DIRS: &[&str] = &[];
/// The tool that prints the developer directory, whose `usr/bin/git` runs
/// when no tool folder has a git: `xcode-select -p`, a real binary that exits 2 without a dialog when none is set.
pub const DEVELOPER_DIR_TOOL: Option<&str> = Some("/usr/bin/xcode-select");
