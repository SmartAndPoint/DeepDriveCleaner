//! The user's home and ddc's own folders under it, on Windows. The one item
//! of this module that reads the environment (ADR "Platform seam: one
//! platform module per OS with declared capabilities", §8):
//!
//! - the home from `USERPROFILE`, or `HOMEDRIVE` + `HOMEPATH` when it is not
//!   set. Never `HOME`, which Git for Windows and MSYS shells set to their own
//!   idea of the home, and never the known-folder API, which ignores the
//!   environment: a test that points these variables at a temporary home must
//!   never reach the user's real profile;
//! - ddc's data from `LOCALAPPDATA`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// `%USERPROFILE%`, or `%HOMEDRIVE%%HOMEPATH%`, when set and not empty.
/// `Env` canonicalizes it.
#[allow(clippy::disallowed_methods)] // the platform's reader of the home; `Env::from_process` is its one caller
pub fn home() -> Option<PathBuf> {
    home_from(std::env::var_os("USERPROFILE"), std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH"))
}

fn home_from(profile: Option<OsString>, drive: Option<OsString>, path: Option<OsString>) -> Option<PathBuf> {
    let set = |v: Option<OsString>| v.filter(|v| !v.is_empty());
    if let Some(profile) = set(profile) {
        return Some(PathBuf::from(profile));
    }
    let mut home = set(drive)?;
    home.push(set(path)?);
    Some(PathBuf::from(home))
}

/// The data and log folders when neither `--data-dir` nor `DDC_DATA_DIR`
/// names one: `%LOCALAPPDATA%\DeepDriveCleaner` and its `logs`. A relative
/// or empty `LOCALAPPDATA` is ignored for the home's `AppData\Local`.
#[allow(clippy::disallowed_methods)] // LOCALAPPDATA, read only here
pub fn default_data_and_log(home: &Path) -> (PathBuf, PathBuf) {
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    data_and_log(home, local.as_deref())
}

fn data_and_log(home: &Path, local: Option<&Path>) -> (PathBuf, PathBuf) {
    let local = match local {
        Some(local) if local.is_absolute() => local.to_path_buf(),
        _ => home.join("AppData").join("Local"),
    };
    let data = local.join("DeepDriveCleaner");
    let logs = data.join("logs");
    (data, logs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_lives_in_local_app_data_and_a_relative_one_is_ignored() {
        let home = Path::new(r"C:\Users\u");
        let default = (
            PathBuf::from(r"C:\Users\u\AppData\Local\DeepDriveCleaner"),
            PathBuf::from(r"C:\Users\u\AppData\Local\DeepDriveCleaner\logs"),
        );
        assert_eq!(data_and_log(home, None), default);
        assert_eq!(data_and_log(home, Some(Path::new("Local"))), default);
        assert_eq!(
            data_and_log(home, Some(Path::new(r"D:\Profiles\u\Local"))),
            (
                PathBuf::from(r"D:\Profiles\u\Local\DeepDriveCleaner"),
                PathBuf::from(r"D:\Profiles\u\Local\DeepDriveCleaner\logs")
            )
        );
    }

    #[test]
    fn the_home_is_the_user_profile_or_the_home_drive_and_path() {
        let s = |v: &str| Some(OsString::from(v));
        assert_eq!(home_from(s(r"C:\Users\u"), s("D:"), s(r"\x")), Some(PathBuf::from(r"C:\Users\u")));
        assert_eq!(home_from(None, s("D:"), s(r"\Users\u")), Some(PathBuf::from(r"D:\Users\u")));
        assert_eq!(home_from(s(""), s("D:"), s(r"\Users\u")), Some(PathBuf::from(r"D:\Users\u")), "an empty profile");
        assert_eq!(home_from(None, s("D:"), None), None);
        assert_eq!(home_from(None, None, s(r"\Users\u")), None);
        assert_eq!(home_from(None, None, None), None);
    }
}
