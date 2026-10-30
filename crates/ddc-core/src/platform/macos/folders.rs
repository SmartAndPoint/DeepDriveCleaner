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
