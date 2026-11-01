//! `ddc keep remove`: removes the `.ddc-keep` in one folder, and nothing
//! else (story "Protected by you: say what it is, and Remove Protection";
//! safety ADR, amendment of 2026-10-30; menu bar ADR §6 item 12). It removes
//! a protection, never the folder: what lay beneath is listed by the next
//! scan in its own group and cleaned there with that group's checks.
//!
//! The caller holds the exclusive lock (a running clean re-checks markers as
//! it acts). In order: the folder must lie in the scan roots; it is opened
//! with `O_NOFOLLOW`; its `(dev, ino)` must be the one the report recorded;
//! its `.ddc-keep` must be a regular file (`fstatat(AT_SYMLINK_NOFOLLOW)`);
//! then it is unlinked relative to that descriptor. Every decision is
//! journaled.

use crate::platform::Canonical as _;
use std::io;
use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::env::Env;
use crate::exec::{KeepJournal, KeepRemoveEntry};
use crate::platform::{self, fsops::DirFd};
use crate::report::ScanOptions;
use crate::rules::KEEP_MARKER;
use crate::scan::WalkSetupError;
use crate::store;

/// The version of the `--json` report (`docs/keep.schema.json`).
pub const KEEP_SCHEMA_VERSION: u32 = 1;

/// What `ddc keep remove --json` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct KeepReport {
    pub schema_version: u32,
    /// The folder that held the marker, as it was given.
    #[serde(serialize_with = "crate::json_path::path")]
    pub path: PathBuf,
    /// The folder's device: the one given with `--folder-dev`, else the one
    /// the open found; `null` when neither is known.
    pub dev: Option<u64>,
    /// The folder's inode, as `dev`.
    pub ino: Option<u64>,
    /// The rule the marker kept, from the saved scan report; `null` when it
    /// does not say.
    pub rule: Option<String>,
    #[serde(flatten)]
    pub result: KeepResult,
}

/// What became of the marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum KeepResult {
    /// The `.ddc-keep` was unlinked.
    Removed,
    /// The folder is there with no `.ddc-keep` in it.
    Absent,
    /// A check refused it: nothing was removed.
    Refused { reason: KeepRefusal },
    /// The open, the stat or the unlink failed.
    Failed { error: String },
}

/// Why a removal was refused. A strict enum: a new reason bumps
/// [`KEEP_SCHEMA_VERSION`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum KeepRefusal {
    /// The folder lies outside the Settings roots.
    OutsideScanRoots,
    /// The folder's identity differs from the one given, or it is now a symlink.
    ChangedSinceScan,
    /// The folder is not there.
    NoFolder,
    /// The `.ddc-keep` is a symlink, a folder or anything but a regular
    /// file: it is removed in Finder.
    NotAFile,
}

/// The folder whose marker is to go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub folder: PathBuf,
    /// `(dev, ino)` as the scan report recorded it; `None` when a person
    /// confirmed the folder at a terminal.
    pub identity: Option<(u64, u64)>,
    /// The rule the marker kept, from the saved report ([`kept_rule`]).
    pub rule: Option<String>,
}

/// An absolute path with no `..`: the roots check reads it as the open
/// does. Anything else is refused before the lock.
pub fn plain(folder: &Path) -> bool {
    folder.is_absolute() && folder.components().all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}

/// The roots a folder must lie in: the Settings roots, or the home folder
/// when there are none, canonical, as `disk free` and the scan resolve them.
pub fn scan_roots(env: &Env, opts: &ScanOptions) -> Result<Vec<PathBuf>, WalkSetupError> {
    crate::scan::normalized_roots(&crate::report::roots_of(env, opts))
}

/// The rule the marker in `folder` kept, from `last-scan.json`: that of the
/// first kept finding that lists the folder among its markers. `None` when
/// the report is absent, unreadable or of another schema version.
pub fn kept_rule(env: &Env, folder: &Path) -> Option<String> {
    let report = store::read_last_scan(env).ok().flatten()?;
    report
        .findings
        .iter()
        .filter_map(|f| f.protection.as_ref())
        .find(|p| p.markers.iter().any(|m| m.folder == folder))
        .and_then(|p| p.rule.clone())
}

/// Removes the `.ddc-keep` of `target.folder` when every check passes, and
/// journals the decision, whatever it is. The caller holds the exclusive
/// lock. An error is the journal's, or the platform's refusal.
pub fn unprotect(target: &Target, roots: &[PathBuf], journal: &mut impl KeepJournal) -> io::Result<KeepReport> {
    platform::require_supported().map_err(|u| io::Error::new(io::ErrorKind::Unsupported, u))?;
    let (identity, result) = decide(&target.folder, target.identity, roots);
    let report = KeepReport {
        schema_version: KEEP_SCHEMA_VERSION,
        path: target.folder.clone(),
        dev: identity.map(|(dev, _)| dev),
        ino: identity.map(|(_, ino)| ino),
        rule: target.rule.clone(),
        result,
    };
    journal.record_keep(&KeepRemoveEntry::new(&report))?;
    Ok(report)
}

/// The checks in order, then the unlink. The identity is the one given, or
/// the one the open found.
fn decide(folder: &Path, given: Option<(u64, u64)>, roots: &[PathBuf]) -> (Option<(u64, u64)>, KeepResult) {
    let refused = |reason| KeepResult::Refused { reason };
    let failed = |e: io::Error| KeepResult::Failed { error: e.to_string() };
    // The folder where its ancestors really lead: a symlink above it cannot
    // carry the open outside the roots.
    let real = match (folder.parent(), folder.file_name()) {
        (Some(parent), Some(name)) => parent.canonical().map_or_else(|_| folder.to_path_buf(), |p| p.join(name)),
        _ => folder.to_path_buf(),
    };
    if !roots.iter().any(|root| real.starts_with(root)) {
        return (given, refused(KeepRefusal::OutsideScanRoots));
    }
    let dir = match DirFd::open(&real) {
        Ok(dir) => dir,
        Err(e) => {
            // `O_NOFOLLOW` refused a symlink (`ELOOP`), a dangling one too.
            let symlink = std::fs::symlink_metadata(&real).is_ok_and(|m| m.file_type().is_symlink());
            let result = match e.kind() {
                _ if symlink => refused(KeepRefusal::ChangedSinceScan),
                io::ErrorKind::NotFound => refused(KeepRefusal::NoFolder),
                // A file now: another entry than the scan's folder.
                io::ErrorKind::NotADirectory if given.is_some() => refused(KeepRefusal::ChangedSinceScan),
                io::ErrorKind::NotADirectory => refused(KeepRefusal::NoFolder),
                _ => failed(e),
            };
            return (given, result);
        }
    };
    let found = match dir.identity() {
        Ok(found) => found,
        Err(e) => return (given, failed(e)),
    };
    if given.is_some_and(|given| given != found) {
        return (given, refused(KeepRefusal::ChangedSinceScan));
    }
    let identity = Some(found);
    let result = match dir.stat_child(KEEP_MARKER.as_ref()) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => KeepResult::Absent,
        Err(e) => failed(e),
        Ok(st) if !st.is_file => refused(KeepRefusal::NotAFile),
        Ok(_) => match dir.unlink_child(KEEP_MARKER.as_ref()) {
            Ok(()) => KeepResult::Removed,
            Err(e) if e.kind() == io::ErrorKind::NotFound => KeepResult::Absent,
            Err(e) => failed(e),
        },
    };
    (identity, result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_absolute_path_without_parent_steps_is_plain() {
        // Its inputs are macOS paths, which Windows does not read as absolute.
        if crate::platform::skipped_without(
            "only_an_absolute_path_without_parent_steps_is_plain",
            crate::platform::testing::posix_paths(),
        )
        .is_none()
        {
            return;
        }
        assert!(plain(Path::new("/a/b")));
        assert!(plain(Path::new("/")));
        // A `.` inside is dropped by `Path` itself, for the roots check and
        // for the open alike.
        assert!(plain(Path::new("/a/./b")));
        for bad in ["a/b", "/a/../b", "/a/b/..", "./a", ""] {
            assert!(!plain(Path::new(bad)), "{bad}");
        }
    }

    #[test]
    fn the_report_writes_its_reason_or_error_beside_the_result() {
        let report = |result| KeepReport {
            schema_version: KEEP_SCHEMA_VERSION,
            path: PathBuf::from("/h/p"),
            dev: Some(1),
            ino: None,
            rule: None,
            result,
        };
        let refused = serde_json::to_value(report(KeepResult::Refused { reason: KeepRefusal::NotAFile })).unwrap();
        assert_eq!(
            refused,
            serde_json::json!({"schema_version": 1, "path": "/h/p", "dev": 1, "ino": null, "rule": null,
                "result": "refused", "reason": "not-a-file"})
        );
        let removed = serde_json::to_value(report(KeepResult::Removed)).unwrap();
        assert!(removed.get("reason").is_none() && removed.get("error").is_none(), "{removed}");
        let failed = serde_json::to_value(report(KeepResult::Failed { error: "EIO".into() })).unwrap();
        assert_eq!((&failed["result"], &failed["error"]), (&"failed".into(), &"EIO".into()));
    }
}
