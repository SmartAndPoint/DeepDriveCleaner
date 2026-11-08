//! The protected folders as one set (ADR "Cleanup safety model", §6, and
//! its bullet on Windows' redirected folders, planned 2026-11-08): the
//! platform's list in the home, the folders the environment redirects, and
//! every ancestor of each.
//!
//! A folder finding is refused only at a protected path, so a rule could
//! claim a folder above a protected one that is not itself listed: the set
//! is closed under ancestors, a home-relative folder's up to the home and a
//! redirected one's up to its volume's root. The folders are protected, not
//! their contents: what lies below an ancestor is walked and found as
//! before. On macOS and Linux every ancestor in the home is listed already.
//!
//! An entry below a redirected folder's default place in the home (Windows'
//! `AppData/Roaming` and `AppData/Local`) is protected below that variable's
//! value too, in each of its spellings, never below the other variable's
//! (the system-folders bullet, planned 2026-11-08).
//!
//! Every check that asks what is protected takes it from here: the catalog,
//! the guard and its entry checks, the dry run and the contents report.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::env::Env;
use crate::platform::types::Redirect;
use crate::platform::{PLATFORM, Platform, paths};

use super::{protected_in_home, redirectable};

/// The protected folders, each as given and by its [`paths::key`].
#[derive(Debug, Clone, Default)]
pub struct Protected {
    folders: Vec<(PathBuf, PathBuf)>,
}

impl Protected {
    /// The set of `env`: its home's list and its redirected folders.
    pub fn of(env: &Env) -> Self {
        Self::new(env.home(), env.redirected())
    }

    /// The platform's list under `home`, and `redirected`, with their ancestors.
    pub fn new(home: &Path, redirected: &[(Redirect, PathBuf)]) -> Self {
        Self::for_platform(PLATFORM, home, redirected)
    }

    /// [`Self::new`] for platform `p`'s list: its tests run on every OS.
    pub(crate) fn for_platform(p: Platform, home: &Path, redirected: &[(Redirect, PathBuf)]) -> Self {
        let list = protected_in_home(p);
        let mut set = Self::default();
        let mut seen = HashSet::new();
        let mut add = |path: &Path| {
            let key = paths::key(path).into_owned();
            if seen.insert(key.clone()) {
                set.folders.push((path.to_path_buf(), key));
            }
        };
        for rel in list {
            // `""` is the home itself.
            let path = if rel.is_empty() { home.to_path_buf() } else { home.join(rel) };
            path.ancestors().take_while(|a| a.starts_with(home)).for_each(&mut add);
        }
        for (var, folder) in redirected {
            folder.ancestors().for_each(&mut add);
            let Some((_, default)) = redirectable(p).iter().find(|(v, _)| v == var) else { continue };
            for rel in list {
                // By components: `AppData/LocalLow` is not below `AppData/Local`.
                if let Ok(rest) = Path::new(rel).strip_prefix(default)
                    && !rest.as_os_str().is_empty()
                {
                    folder.join(rest).ancestors().for_each(&mut add);
                }
            }
        }
        set
    }

    /// The folders as given.
    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.folders.iter().map(|(p, _)| p.as_path())
    }

    /// Whether `path` is a protected folder.
    pub fn contains(&self, path: &Path) -> bool {
        let key = paths::key(path);
        self.folders.iter().any(|(_, k)| *k == *key)
    }

    /// Whether a protected folder is `path` or lies below it: an entry that
    /// holds one is never removed.
    pub fn at_or_below(&self, path: &Path) -> bool {
        let key = paths::key(path);
        self.folders.iter().any(|(_, k)| k.starts_with(&key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(p: &str) -> PathBuf {
        PathBuf::from(p)
    }

    #[test]
    fn the_closure_adds_nothing_to_the_macos_and_linux_lists_and_the_windows_ancestors() {
        let home = Path::new("/home/example");
        for p in [Platform::Macos, Platform::Linux, Platform::Windows] {
            let set = Protected::for_platform(p, home, &[]);
            let listed: HashSet<PathBuf> = protected_in_home(p)
                .iter()
                .map(|rel| if rel.is_empty() { home.to_path_buf() } else { home.join(rel) })
                .collect();
            let all: HashSet<PathBuf> = set.paths().map(Path::to_path_buf).collect();
            assert!(all.is_superset(&listed), "{p}");
            assert!(all.iter().all(|f| f.starts_with(home)), "{p}: nothing above the home");
            let added: HashSet<&PathBuf> = all.difference(&listed).collect();
            if p == Platform::Windows {
                let expected: HashSet<PathBuf> = [
                    "AppData/Roaming/Microsoft",
                    "AppData/Roaming/Mozilla",
                    "AppData/Roaming/Mozilla/Firefox",
                    "AppData/Roaming/Thunderbird",
                    "AppData/Local/Microsoft",
                    "AppData/Local/Microsoft/Windows",
                    "AppData/Local/Google",
                    "AppData/Local/Google/Chrome",
                    "AppData/Local/Microsoft/Edge",
                    "AppData/Local/BraveSoftware",
                    "AppData/Local/BraveSoftware/Brave-Browser",
                ]
                .iter()
                .map(|rel| home.join(rel))
                .collect();
                assert_eq!(added, expected.iter().collect(), "{p}");
            } else {
                assert!(added.is_empty(), "{p}: the closure adds nothing: {added:?}");
            }
        }
    }

    #[test]
    fn the_windows_entries_below_appdata_move_below_their_own_variables_value() {
        let home = Path::new("/home/u");
        let (roaming, local) = (at("/d/Profiles/u/Roaming"), at("/e/Local"));
        let set = Protected::for_platform(
            Platform::Windows,
            home,
            &[(Redirect::Roaming, roaming.clone()), (Redirect::Local, local.clone())],
        );
        for rel in ["Microsoft/Protect", "Mozilla/Firefox/Profiles", "Thunderbird/Profiles", "gnupg"] {
            assert!(set.contains(&roaming.join(rel)), "{rel} below APPDATA");
            assert!(!set.contains(&local.join(rel)), "{rel} never below LOCALAPPDATA");
        }
        assert!(set.contains(&roaming.join("Microsoft")) && set.contains(&local.join("Microsoft")), "both hold one");
        for rel in ["Packages", "wsl", "Microsoft/Vault", "Microsoft/Windows/INetCookies", "Google/Chrome/User Data"] {
            assert!(set.contains(&local.join(rel)), "{rel} below LOCALAPPDATA");
            assert!(!set.contains(&roaming.join(rel)), "{rel} never below APPDATA");
        }
        assert!(set.contains(&home.join("AppData/Roaming/Microsoft/Protect")), "its default place stays");
        assert!(!set.contains(&local.join("LocalLow")) && !set.contains(Path::new("/e/LocalLow")), "LocalLow stays");
        assert!(!set.contains(&roaming.join("npm-cache")), "nothing else below it");
        assert!(set.contains(Path::new("/d/Profiles")) && set.contains(Path::new("/")), "the value's ancestors");
    }

    #[test]
    fn the_home_list_is_closed_under_ancestors_already_on_macos_and_linux() {
        let home = Path::new("/home/example");
        let set = Protected::new(home, &[]);
        let listed: HashSet<PathBuf> = protected_in_home(PLATFORM)
            .iter()
            .map(|rel| if rel.is_empty() { home.to_path_buf() } else { home.join(rel) })
            .map(|p| paths::key(&p).into_owned())
            .collect();
        let all: HashSet<PathBuf> = set.paths().map(|p| paths::key(p).into_owned()).collect();
        if PLATFORM != crate::platform::Platform::Windows {
            assert_eq!(all, listed, "the closure adds nothing here");
        }
        assert!(all.is_superset(&listed));
        assert!(set.paths().all(|p| p.starts_with(home)), "nothing above the home");
    }

    #[test]
    fn a_redirected_folder_is_protected_with_every_ancestor_and_not_its_contents() {
        let set =
            Protected::new(Path::new("/home/example"), &[(Redirect::Roaming, at("/elsewhere/profiles/u/Roaming"))]);
        for folder in
            ["/elsewhere/profiles/u/Roaming", "/elsewhere/profiles/u", "/elsewhere/profiles", "/elsewhere", "/"]
        {
            assert!(set.contains(Path::new(folder)), "{folder}");
        }
        assert!(!set.contains(Path::new("/elsewhere/profiles/u/Roaming/npm-cache")), "its contents are not");
        assert!(!set.contains(Path::new("/elsewhere/profiles/v")), "a sibling is not");
        assert!(set.at_or_below(Path::new("/elsewhere/profiles")), "an entry holding it");
        assert!(!set.at_or_below(Path::new("/elsewhere/profiles/u/Roaming/npm-cache")));
    }

    #[test]
    fn a_redirected_folder_inside_the_home_is_protected_too() {
        let home = Path::new("/home/example");
        let set = Protected::new(home, &[(Redirect::Roaming, home.join("Profiles/Roaming"))]);
        assert!(set.contains(&home.join("Profiles/Roaming")));
        assert!(set.contains(&home.join("Profiles")));
        assert!(!set.contains(&home.join("Profiles/Roaming/x")));
    }
}
