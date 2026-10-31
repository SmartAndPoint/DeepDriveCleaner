//! The user's home and ddc's own folders under it, on Linux. The one item of
//! this module that reads the environment: `HOME`, and `XDG_STATE_HOME` for
//! ddc's own data (ADR "Platform seam: one platform module per OS with
//! declared capabilities", §8; safety ADR, amendment of 2026-10-31).

use std::path::{Path, PathBuf};

/// `$HOME`, when set and not empty. `Env` canonicalizes it.
#[allow(clippy::disallowed_methods)] // the platform's reader of HOME; `Env::from_process` is its one caller
pub fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|h| !h.is_empty()).map(PathBuf::from)
}

/// The data and log folders when neither `--data-dir` nor `DDC_DATA_DIR`
/// names one: `$XDG_STATE_HOME/deepdrivecleaner` and its `logs`. A relative
/// `XDG_STATE_HOME` is ignored, as the XDG specification says.
#[allow(clippy::disallowed_methods)] // XDG_STATE_HOME, read only here
pub fn default_data_and_log(home: &Path) -> (PathBuf, PathBuf) {
    let state = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from);
    data_and_log(home, state.as_deref())
}

fn data_and_log(home: &Path, state: Option<&Path>) -> (PathBuf, PathBuf) {
    let data = match state {
        Some(state) if state.is_absolute() => state.join("deepdrivecleaner"),
        _ => home.join(".local/state/deepdrivecleaner"),
    };
    let logs = data.join("logs");
    (data, logs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_lives_in_the_xdg_state_folder_and_a_relative_one_is_ignored() {
        let home = Path::new("/home/u");
        let default = (
            PathBuf::from("/home/u/.local/state/deepdrivecleaner"),
            PathBuf::from("/home/u/.local/state/deepdrivecleaner/logs"),
        );
        assert_eq!(data_and_log(home, None), default);
        assert_eq!(data_and_log(home, Some(Path::new("state"))), default);
        assert_eq!(
            data_and_log(home, Some(Path::new("/var/u/state"))),
            (PathBuf::from("/var/u/state/deepdrivecleaner"), PathBuf::from("/var/u/state/deepdrivecleaner/logs"))
        );
    }
}
