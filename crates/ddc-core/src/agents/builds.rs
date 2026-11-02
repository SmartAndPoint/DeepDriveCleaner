//! A coding agent's old builds: the picked entries of Claude Code's and
//! Cursor's agent's `versions` folders (safety ADR, amendment of 2026-11-02,
//! story "Coding agents on Linux: ddc disk agents", DDC-010). Which build is
//! still needed is decided here once per folder, from what the scan, the dry
//! run and the guard each read again: the launcher's target, the builds the
//! user's processes run, Claude Code's installer locks and the newest. What
//! cannot be told locks every build of the harness.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::platform::Canonical as _;
use crate::platform::procs::ProcInfo;

/// Claude Code's per-build installer locks, from the home folder.
pub const CLAUDE_LOCKS: &str = ".local/state/claude/locks";

/// Whose builds a folder holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildHarness {
    /// `~/.local/share/claude/versions`: one file per build, named by its version.
    ClaudeCode,
    /// `~/.local/share/cursor-agent/versions`: one folder per build, `<date>-<hash>`.
    CursorAgent,
}

impl BuildHarness {
    /// The launchers, from the home folder: the build one of them points to is in use.
    fn launchers(self) -> &'static [&'static str] {
        match self {
            Self::ClaudeCode => &[".local/bin/claude"],
            Self::CursorAgent => &[".local/bin/agent", ".local/bin/cursor-agent"],
        }
    }

    /// The names a process of the harness goes by: its kernel name or its first argument's file name.
    fn names(self) -> &'static [&'static str] {
        match self {
            Self::ClaudeCode => &["claude"],
            Self::CursorAgent => &["cursor-agent", "agent"],
        }
    }

    /// Whether a process named `name` may be this harness. A native Claude
    /// Code started through its launcher may go by its build's file name, a
    /// bare version (`2.1.126`), as its kernel name: such a name counts too,
    /// which fails closed.
    fn goes_by(self, name: &str) -> bool {
        self.names().contains(&name) || (self == Self::ClaudeCode && bare_version(name.as_bytes()))
    }

    /// Whether `name` (of a regular file, or of a folder when `is_dir`) is a build.
    pub fn is_build(self, name: &[u8], is_dir: bool) -> bool {
        match self {
            Self::ClaudeCode => !is_dir && bare_version(name),
            Self::CursorAgent => is_dir && date_hash(name),
        }
    }

    /// The key "newest" orders by: the version, or the build's date.
    fn order_key(self, name: &str) -> Vec<u64> {
        let key = match self {
            Self::ClaudeCode => name,
            Self::CursorAgent => name.split('-').next().unwrap_or(name),
        };
        crate::rules::toolchain::version_key(key)
    }
}

/// `2.1.126`: digits in dot-separated parts, at least two.
fn bare_version(name: &[u8]) -> bool {
    let parts: Vec<&[u8]> = name.split(|b| *b == b'.').collect();
    parts.len() >= 2 && parts.iter().all(|p| !p.is_empty() && p.iter().all(u8::is_ascii_digit))
}

/// `2026.10.22-27dcf5f`: a dotted date, a dash, a lowercase hex hash.
fn date_hash(name: &[u8]) -> bool {
    let Some(dash) = name.iter().position(|b| *b == b'-') else { return false };
    let (date, hash) = (&name[..dash], &name[dash + 1..]);
    bare_version(date) && !hash.is_empty() && hash.iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

/// Why a build is in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum BuildBy {
    /// A launcher points to it.
    Launcher,
    /// A process of the user runs it.
    Process,
    /// Claude Code's installer lock names a live process for it.
    Lock,
    /// The newest build.
    Newest,
}

/// Why no build of a harness can be told free: each locks them all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum BuildsCause {
    /// No launcher: the build in use cannot be told.
    NoLauncher,
    /// The launcher does not resolve inside the folder (a custom one, or
    /// builds moved by `XDG_DATA_HOME`).
    CustomLauncher,
    /// A process of the user named as the harness could not be inspected.
    ProcessWithheld,
    /// A readable process of another user carries the harness's name.
    OtherUserProcess,
    /// `/proc` hides other users' processes (Linux `hidepid`).
    HiddenProcessTable,
    /// The home is on a network filesystem, which other machines may run builds from.
    NetworkHome,
}

/// What the machine tells beyond the user's own processes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// Other users' process names (`platform::procs::other_user_names`).
    pub others: Vec<String>,
    /// `/proc` hides other users' processes.
    pub hidden: bool,
    /// The home's filesystem type; `None` when it could not be read.
    pub home_fs: Option<String>,
}

impl Host {
    /// Read from the system now.
    pub fn read(home: &Path) -> Self {
        Self {
            others: crate::platform::procs::other_user_names(),
            hidden: crate::platform::procs::process_table_hidden(),
            home_fs: crate::platform::statfs::statfs(home).ok().map(|s| s.fs_type),
        }
    }
}

/// Each build's use by name, or why no build's use can be told.
pub type BuildsUse = Result<HashMap<OsString, Option<BuildBy>>, BuildsCause>;

/// The state of each build of `folder`, by name; `Err` locks them all.
/// `Ok(None)` for a name is a free build. `names` are the folder's children
/// that are builds; `procs` the user's processes; `lock_live` tells whether a
/// pid of any user runs.
pub fn decide(
    harness: BuildHarness,
    home: &Path,
    folder: &Path,
    names: &[OsString],
    procs: &[ProcInfo],
    lock_live: &dyn Fn(i32) -> bool,
    host: &Host,
) -> BuildsUse {
    if host.hidden {
        return Err(BuildsCause::HiddenProcessTable);
    }
    if host.home_fs.as_deref().is_none_or(crate::platform::statfs::is_network_fs) {
        return Err(BuildsCause::NetworkHome);
    }
    if host.others.iter().any(|n| harness.goes_by(n)) {
        return Err(BuildsCause::OtherUserProcess);
    }
    let named = |p: &ProcInfo| {
        let first = p.args.first().and_then(|a| Path::new(a).file_name()).and_then(OsStr::to_str);
        harness.goes_by(&p.name) || first.is_some_and(|f| harness.goes_by(f))
    };
    if procs.iter().any(|p| named(p) && (p.withheld || p.resolved_exe.is_none())) {
        return Err(BuildsCause::ProcessWithheld);
    }
    let folder = folder.canonical().unwrap_or_else(|_| folder.to_path_buf());
    let mut found = false;
    let mut targets: Vec<OsString> = Vec::new();
    for launcher in harness.launchers() {
        let at = home.join(launcher);
        if at.symlink_metadata().is_err() {
            continue;
        }
        found = true;
        if let Some(build) = at.canonical().ok().and_then(|t| build_of(&folder, &t)) {
            targets.push(build);
        }
    }
    if !found {
        return Err(BuildsCause::NoLauncher);
    }
    if targets.is_empty() {
        return Err(BuildsCause::CustomLauncher);
    }
    let running: Vec<OsString> = procs
        .iter()
        .filter_map(|p| p.resolved_exe.as_deref().or(p.exe.as_deref()))
        .filter_map(|exe| build_of(&folder, exe))
        .collect();
    let newest = names
        .iter()
        .filter_map(|n| n.to_str())
        .max_by(|a, b| harness.order_key(a).cmp(&harness.order_key(b)).then_with(|| a.cmp(b)))
        .map(OsString::from);
    let locks = home.join(CLAUDE_LOCKS);
    Ok(names
        .iter()
        .map(|name| {
            let by = if targets.contains(name) {
                Some(BuildBy::Launcher)
            } else if running.contains(name) {
                Some(BuildBy::Process)
            } else if harness == BuildHarness::ClaudeCode && locked(&locks, name, lock_live) {
                Some(BuildBy::Lock)
            } else if newest.as_ref() == Some(name) {
                Some(BuildBy::Newest)
            } else {
                None
            };
            (name.clone(), by)
        })
        .collect())
}

/// The build `path` lies in: the first component under `folder`.
fn build_of(folder: &Path, path: &Path) -> Option<OsString> {
    path.strip_prefix(folder).ok()?.components().next().map(|c| c.as_os_str().to_os_string())
}

/// `locks/<build>.lock` names a live pid. A lock that exists but cannot be
/// read, or names no pid, counts as live: it fails closed.
fn locked(locks: &Path, name: &OsStr, lock_live: &dyn Fn(i32) -> bool) -> bool {
    let mut file = name.to_os_string();
    file.push(".lock");
    let path: PathBuf = locks.join(file);
    match std::fs::read(&path) {
        Ok(text) => serde_json::from_slice::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("pid")?.as_i64())
            .and_then(|pid| i32::try_from(pid).ok())
            .is_none_or(lock_live),
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::platform::{skipped_without, testing};

    #[test]
    fn a_build_is_named_as_its_harness_names_one() {
        let claude = BuildHarness::ClaudeCode;
        assert!(claude.is_build(b"2.1.126", false));
        assert!(!claude.is_build(b"2.1.126", true), "a folder is no Claude Code build");
        assert!(!claude.is_build(b"2.1.126.tmp.123.456", false), "an installer's temporary file");
        assert!(!claude.is_build(b"2", false) && !claude.is_build(b"2..1", false) && !claude.is_build(b"x.y", false));
        let cursor = BuildHarness::CursorAgent;
        assert!(cursor.is_build(b"2026.10.22-27dcf5f", true));
        assert!(!cursor.is_build(b"2026.10.22-27dcf5f", false), "a file is no Cursor build");
        assert!(!cursor.is_build(b".tmp-2026.10.22-27dcf5f-1730000000", true));
        assert!(!cursor.is_build(b"2026.10.22-", true) && !cursor.is_build(b"2026.10.22-XYZ", true));
    }

    #[test]
    fn newest_orders_by_version_and_then_by_date() {
        let c = BuildHarness::ClaudeCode;
        assert!(c.order_key("2.1.10") > c.order_key("2.1.9"), "numeric, not by string");
        let k = BuildHarness::CursorAgent;
        assert!(k.order_key("2026.10.22-aa") > k.order_key("2026.9.30-ff"));
        assert_eq!(k.order_key("2026.10.22-aa"), k.order_key("2026.10.22-bb"), "a tie goes by name");
    }

    fn host() -> Host {
        Host { others: Vec::new(), hidden: false, home_fs: Some("ext4".to_owned()) }
    }

    fn proc(name: &str, resolved: Option<PathBuf>) -> ProcInfo {
        ProcInfo {
            pid: 42,
            name: name.to_owned(),
            exe: resolved.clone(),
            resolved_exe: resolved,
            ..ProcInfo::default()
        }
    }

    /// A home with Claude Code's builds, the launcher pointing to `current`.
    fn claude_home(builds: &[&str], current: Option<&str>) -> (tempfile::TempDir, PathBuf, Vec<OsString>) {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().canonical().unwrap();
        let versions = root.join(".local/share/claude/versions");
        fs::create_dir_all(&versions).unwrap();
        fs::create_dir_all(root.join(".local/bin")).unwrap();
        for b in builds {
            fs::write(versions.join(b), b"x").unwrap();
        }
        if let Some(c) = current {
            testing::symlink(&versions.join(c), &root.join(".local/bin/claude")).unwrap();
        }
        let names = builds.iter().map(OsString::from).collect();
        (home, root, names)
    }

    #[test]
    fn the_launchers_target_a_running_build_a_locked_one_and_the_newest_are_in_use() {
        if skipped_without(
            "the_launchers_target_a_running_build_a_locked_one_and_the_newest_are_in_use",
            testing::posix_paths(),
        )
        .is_none()
        {
            return;
        }
        let (_t, home, names) = claude_home(&["2.1.100", "2.1.110", "2.1.120", "2.1.9", "2.1.130"], Some("2.1.120"));
        let versions = home.join(".local/share/claude/versions");
        let locks = home.join(CLAUDE_LOCKS);
        fs::create_dir_all(&locks).unwrap();
        fs::write(locks.join("2.1.100.lock"), br#"{"pid":7,"version":"2.1.100"}"#).unwrap();
        let procs = [proc("2.1.110", Some(versions.join("2.1.110")))];
        let live = |pid: i32| pid == 7;
        let got = decide(BuildHarness::ClaudeCode, &home, &versions, &names, &procs, &live, &host()).unwrap();
        assert_eq!(got[OsStr::new("2.1.120")], Some(BuildBy::Launcher));
        assert_eq!(got[OsStr::new("2.1.110")], Some(BuildBy::Process));
        assert_eq!(got[OsStr::new("2.1.100")], Some(BuildBy::Lock));
        assert_eq!(got[OsStr::new("2.1.130")], Some(BuildBy::Newest));
        assert_eq!(got[OsStr::new("2.1.9")], None, "the one old build nothing needs");
        // A lock whose pid is gone frees its build; one that cannot be read does not.
        let dead = |_: i32| false;
        let got = decide(BuildHarness::ClaudeCode, &home, &versions, &names, &procs, &dead, &host()).unwrap();
        assert_eq!(got[OsStr::new("2.1.100")], None);
        fs::write(locks.join("2.1.9.lock"), b"not json").unwrap();
        let got = decide(BuildHarness::ClaudeCode, &home, &versions, &names, &procs, &dead, &host()).unwrap();
        assert_eq!(got[OsStr::new("2.1.9")], Some(BuildBy::Lock), "an unreadable lock fails closed");
    }

    #[test]
    fn what_cannot_be_told_locks_every_build() {
        if skipped_without("what_cannot_be_told_locks_every_build", testing::posix_paths()).is_none() {
            return;
        }
        let (_t, home, names) = claude_home(&["2.1.1", "2.1.2"], Some("2.1.2"));
        let versions = home.join(".local/share/claude/versions");
        let none = |_: i32| false;
        let at = |procs: &[ProcInfo], host: &Host| {
            decide(BuildHarness::ClaudeCode, &home, &versions, &names, procs, &none, host).err()
        };
        assert_eq!(at(&[], &host()), None);
        assert_eq!(at(&[], &Host { hidden: true, ..host() }), Some(BuildsCause::HiddenProcessTable));
        assert_eq!(at(&[], &Host { home_fs: Some("nfs".to_owned()), ..host() }), Some(BuildsCause::NetworkHome));
        assert_eq!(at(&[], &Host { home_fs: None, ..host() }), Some(BuildsCause::NetworkHome), "unread: closed");
        assert_eq!(at(&[], &Host { others: vec!["claude".to_owned()], ..host() }), Some(BuildsCause::OtherUserProcess));
        assert_eq!(at(&[], &Host { others: vec!["ssh-agent".to_owned()], ..host() }), None);
        assert_eq!(
            at(&[], &Host { others: vec!["2.1.126".to_owned()], ..host() }),
            Some(BuildsCause::OtherUserProcess),
            "a native Claude Code may go by its build's version"
        );
        let withheld = ProcInfo { withheld: true, ..proc("claude", None) };
        assert_eq!(at(&[withheld], &host()), Some(BuildsCause::ProcessWithheld));
        let other = ProcInfo { withheld: true, ..proc("ssh-agent", None) };
        assert_eq!(at(&[other], &host()), None, "a withheld process of another name is only counted");
        let by_arg = ProcInfo { args: vec!["/usr/local/bin/claude".into()], ..proc("2.1.2", None) };
        assert_eq!(at(&[by_arg], &host()), Some(BuildsCause::ProcessWithheld), "named by its first argument");

        fs::rename(home.join(".local/bin/claude"), home.join(".local/bin/claude.moved")).unwrap();
        assert_eq!(at(&[], &host()), Some(BuildsCause::NoLauncher));
        fs::write(home.join(".local/bin/claude"), b"#!/bin/sh\n").unwrap();
        assert_eq!(at(&[], &host()), Some(BuildsCause::CustomLauncher));
    }

    #[test]
    fn cursors_builds_are_folders_and_either_launcher_counts() {
        if skipped_without("cursors_builds_are_folders_and_either_launcher_counts", testing::posix_paths()).is_none() {
            return;
        }
        let t = tempfile::tempdir().unwrap();
        let home = t.path().canonical().unwrap();
        let versions = home.join(".local/share/cursor-agent/versions");
        for b in ["2026.9.30-aa11", "2026.10.22-bb22", "2026.10.1-cc33"] {
            fs::create_dir_all(versions.join(b)).unwrap();
            fs::write(versions.join(b).join("cursor-agent"), b"x").unwrap();
        }
        fs::create_dir_all(home.join(".local/bin")).unwrap();
        testing::symlink(&home.join("elsewhere"), &home.join(".local/bin/agent")).unwrap();
        testing::symlink(&versions.join("2026.10.1-cc33/cursor-agent"), &home.join(".local/bin/cursor-agent")).unwrap();
        let names: Vec<OsString> =
            ["2026.9.30-aa11", "2026.10.22-bb22", "2026.10.1-cc33"].iter().map(OsString::from).collect();
        let running = [proc("node", Some(versions.join("2026.9.30-aa11/cursor-agent")))];
        let none = |_: i32| false;
        let got = decide(BuildHarness::CursorAgent, &home, &versions, &names, &running, &none, &host()).unwrap();
        assert_eq!(got[OsStr::new("2026.10.1-cc33")], Some(BuildBy::Launcher), "one launcher resolves inside");
        assert_eq!(got[OsStr::new("2026.9.30-aa11")], Some(BuildBy::Process));
        assert_eq!(got[OsStr::new("2026.10.22-bb22")], Some(BuildBy::Newest));
    }
}
