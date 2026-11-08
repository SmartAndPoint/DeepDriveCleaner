//! The user's home and ddc's own folders under it, on Windows. The one item
//! of this module that reads the environment (ADR "Platform seam: one
//! platform module per OS with declared capabilities", §8):
//!
//! - the home from `USERPROFILE`, or `HOMEDRIVE` + `HOMEPATH` when it is not
//!   set. Never `HOME`, which Git for Windows and MSYS shells set to their own
//!   idea of the home, and never the known-folder API, which ignores the
//!   environment: a test that points these variables at a temporary home must
//!   never reach the user's real profile;
//! - ddc's data from `LOCALAPPDATA`;
//! - `APPDATA` and `LOCALAPPDATA` as folders to protect wherever they point
//!   (safety ADR §6, Windows, redirected folders, planned 2026-11-08).
//!
//! It is also the one place that calls the known-folder API, for five
//! machine-wide folders only, which the guard protects: Windows, Program
//! Files, Program Files (x86), ProgramData and the profiles folder (safety
//! ADR §6, Windows, system folders, planned 2026-11-08; seam ADR §8). Never
//! a per-user folder: those follow the environment. A structure test holds it.

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};

use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::{
    FOLDERID_ProgramData, FOLDERID_ProgramFiles, FOLDERID_ProgramFilesX86, FOLDERID_UserProfiles, FOLDERID_Windows,
    KF_FLAG_DEFAULT, SHGetKnownFolderPath,
};
use windows_sys::core::GUID;

use crate::platform::types::Redirect;

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

/// `APPDATA` and `LOCALAPPDATA`, each with its [`Redirect`], when set to an
/// absolute path, with a `\\?\` prefix dropped (the spelling a scan
/// reports): the folders `Env` protects wherever they point, with their
/// ancestors. `Env` adds their canonical spellings on a local volume.
#[allow(clippy::disallowed_methods)] // APPDATA and LOCALAPPDATA, read only here
pub fn redirected() -> Vec<(Redirect, PathBuf)> {
    redirected_from([std::env::var_os("APPDATA"), std::env::var_os("LOCALAPPDATA")])
}

fn redirected_from([roaming, local]: [Option<OsString>; 2]) -> Vec<(Redirect, PathBuf)> {
    [(Redirect::Roaming, roaming), (Redirect::Local, local)]
        .into_iter()
        .filter_map(|(var, value)| Some((var, PathBuf::from(value?))))
        .filter(|(_, p)| p.is_absolute())
        .map(|(var, p)| (var, super::paths::simplified(p)))
        .collect()
}

/// The machine's own system folders, as the known-folder API names them,
/// and each one's drive root: what the guard adds to the static `C:\` list,
/// so a Windows on another drive is protected too. A folder the call cannot
/// give is left out; the static list stays.
pub fn system_folders() -> Vec<PathBuf> {
    let ids = [
        FOLDERID_Windows,
        FOLDERID_ProgramFiles,
        FOLDERID_ProgramFilesX86,
        FOLDERID_ProgramData,
        FOLDERID_UserProfiles,
    ];
    let mut folders: Vec<PathBuf> = ids.iter().filter_map(known_folder).collect();
    let roots: Vec<PathBuf> = folders.iter().filter_map(|f| f.ancestors().last().map(Path::to_path_buf)).collect();
    for root in roots {
        if !folders.contains(&root) {
            folders.push(root);
        }
    }
    folders
}

fn known_folder(id: &GUID) -> Option<PathBuf> {
    let mut path = std::ptr::null_mut();
    // SAFETY: `id` is a valid GUID, no token is passed (the caller's own), and
    // `path` receives a string the API allocates, freed below in every case.
    let hr = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT as _, std::ptr::null_mut(), &mut path) };
    let folder = (hr >= 0 && !path.is_null()).then(|| {
        // SAFETY: on success `path` is a NUL-terminated wide string.
        let len = unsafe { (0..).take_while(|&i| *path.add(i) != 0).count() };
        // SAFETY: `len` wide characters before the NUL, all readable.
        PathBuf::from(OsString::from_wide(unsafe { std::slice::from_raw_parts(path, len) }))
    });
    // SAFETY: the API's allocation (or null), freed once.
    unsafe { CoTaskMemFree(path.cast()) };
    folder.filter(|f| f.is_absolute())
}

/// The program of a git probe (story "Worktree findings and provenance",
/// decision 10).
pub const GIT_PROGRAM: &str = "git.exe";
/// Folders whose git is never run: none here.
pub const GIT_SHIM_DIRS: &[&str] = &[];
/// Searched for git after the tool folders.
pub const GIT_SYSTEM_DIRS: &[&str] = &[];
/// The tool that prints the developer directory, whose `usr/bin/git` runs
/// when no tool folder has a git: none here.
pub const DEVELOPER_DIR_TOOL: Option<&str> = None;

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
    fn the_redirected_folders_are_the_absolute_values_simplified_with_their_variable() {
        let s = |v: &str| Some(OsString::from(v));
        let r = |v: &str| (Redirect::Roaming, PathBuf::from(v));
        let l = |v: &str| (Redirect::Local, PathBuf::from(v));
        assert_eq!(redirected_from([None, None]), []);
        assert_eq!(redirected_from([s(""), s("Local")]), [], "empty and relative");
        assert_eq!(
            redirected_from([s(r"D:\Profiles\u\Roaming"), s(r"C:\Users\u\AppData\Local")]),
            [r(r"D:\Profiles\u\Roaming"), l(r"C:\Users\u\AppData\Local")]
        );
        assert_eq!(redirected_from([None, s(r"\\srv\share\u\Local")]), [l(r"\\srv\share\u\Local")]);
        assert_eq!(
            redirected_from([s(r"\\?\D:\Roaming"), s(r"\\?\UNC\srv\share\Local")]),
            [r(r"D:\Roaming"), l(r"\\srv\share\Local")]
        );
    }

    #[test]
    fn the_system_folders_are_absolute_exist_and_hold_the_system_drives_root() {
        let folders = system_folders();
        assert!(folders.len() >= 5, "{folders:?}");
        assert!(folders.iter().all(|f| f.is_absolute() && f.exists()), "{folders:?}");
        let windows = known_folder(&FOLDERID_Windows).expect("the Windows folder");
        let root = windows.ancestors().last().unwrap();
        assert!(folders.iter().any(|f| f == root), "{root:?} in {folders:?}");
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
