//! Entries of the user's own folders (story "User data: pick your own files
//! for the Trash"; safety ADR, amendment of 2026-10-20): which rules list
//! their direct children, and what locks one. [`lock_of`] is the one
//! derivation the scan, the dry run and the executor's guard share.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Rules whose findings list their direct children as entries. iMazing
/// backups stay report only until their layout is checked.
pub const ENTRY_RULES: &[&str] = &["user.downloads", "user.pictures", "user.movies", "user.music", "user.ios-backups"];

/// Rules whose sessions are their entries, to the Trash one by one (ADR
/// "Agent footprint: worktrees, harness data and bounded consent for
/// agents", §3). Not [`ENTRY_RULES`], which means the user's own folders;
/// until their entry policy lands they are report only.
pub const SESSION_ENTRY_RULES: &[&str] = &["agent.claude-code-file-history", "agent.claude-code-transcripts"];

/// Whether `rule`'s findings have entries a person can pick: the user's
/// own folders and the coding agents' session folders.
pub fn has_entries(rule: &str) -> bool {
    ENTRY_RULES.contains(&rule) || SESSION_ENTRY_RULES.contains(&rule)
}

/// The rule whose entries are device backups.
pub const BACKUP_RULE: &str = "user.ios-backups";

/// Added or changed this close to the scan's start, either side: in use or
/// still arriving. Further ahead is a wrong clock, not activity.
pub const RECENT: Duration = Duration::from_secs(600);

/// Library packages an app manages, by extension (ASCII case-insensitive).
const APP_EXTENSIONS: &[(&str, &str)] = &[
    ("photoslibrary", "Photos"),
    ("photolibrary", "Photos"),
    ("musiclibrary", "Music"),
    ("tvlibrary", "TV"),
    ("imovielibrary", "iMovie"),
    ("fcpbundle", "Final Cut Pro"),
    ("lrcat", "Lightroom Classic"),
    ("lrcat-data", "Lightroom Classic"),
    ("lrcat-wal", "Lightroom Classic"),
    ("lrdata", "Lightroom Classic"),
    ("lrlibrary", "Lightroom"),
    ("aplibrary", "Aperture"),
    ("cocatalog", "Capture One"),
    ("cosessiondb", "Capture One"),
];

/// Folders an app manages, relative to the home folder.
const APP_PATHS: &[(&str, &str)] = &[
    ("Music/Music", "Music"),
    // A library upgraded from an older macOS keeps its media here.
    ("Music/iTunes", "Music"),
    ("Music/GarageBand", "GarageBand"),
    ("Music/Audio Music Apps", "Logic Pro"),
    ("Movies/TV", "TV"),
    ("Movies/Motion Templates.localized", "Motion"),
    ("Movies/Final Cut Backups.localized", "Final Cut Pro"),
    ("Pictures/Photo Booth Library", "Photo Booth"),
    // Lightroom Classic keeps its catalog one level down.
    ("Pictures/Lightroom", "Lightroom Classic"),
];

/// Downloads still arriving: Safari's package, Chrome's and Firefox's files.
const PARTIAL: &[&str] = &["download", "crdownload", "part"];

/// Why an entry cannot be picked. One type in the scan report and the clean report.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum EntryLock {
    /// An app manages it; clean it in that app.
    AppManaged { app: String },
    /// A `.ddc-keep` in it, or an exclude at or in it.
    Kept,
    /// A protected folder (safety ADR §6) at or inside it.
    Protected,
    /// Another volume is mounted inside it.
    OtherVolume,
    /// Added or changed within [`RECENT`] of the scan's start: in use or still arriving.
    Recent,
    /// A download still arriving.
    PartialDownload,
    /// A backup whose `Status.plist` does not say it finished.
    UnfinishedBackup,
}

/// What [`lock_of`] decides from; the scan, the dry run and the guard gather it each their own way.
#[derive(Debug, Clone)]
pub struct EntryFacts<'a> {
    pub home: &'a Path,
    pub rule: &'a str,
    pub path: &'a Path,
    /// Unix seconds; `None` where the filesystem did not report it.
    pub modified: Option<i64>,
    pub added: Option<i64>,
    /// The scan's start (`Env::now`).
    pub now: SystemTime,
    /// A `.ddc-keep` in it, an exclude at or in it, or a `user.kept` finding inside.
    pub kept: bool,
    pub protected: bool,
    pub other_volume: bool,
    /// For a backup: whether its `Status.plist` says it finished.
    pub finished: bool,
}

/// The entry's lock, permanent reasons first and "recent", which clears by
/// itself, last.
pub fn lock_of(f: &EntryFacts<'_>) -> Option<EntryLock> {
    let name = f.path.file_name().map(|n| n.as_encoded_bytes()).unwrap_or_default();
    if f.protected {
        return Some(EntryLock::Protected);
    }
    if let Some(app) = app_managing(f.home, f.path, name) {
        return Some(EntryLock::AppManaged { app: app.to_owned() });
    }
    if f.kept {
        return Some(EntryLock::Kept);
    }
    if f.other_volume {
        return Some(EntryLock::OtherVolume);
    }
    if f.rule == BACKUP_RULE && !f.finished {
        return Some(EntryLock::UnfinishedBackup);
    }
    if PARTIAL.iter().any(|ext| has_extension(name, ext)) {
        return Some(EntryLock::PartialDownload);
    }
    if recent(f.modified, f.added, f.now) {
        return Some(EntryLock::Recent);
    }
    None
}

/// Within [`RECENT`] of `now`, either side. A time not reported is left out;
/// with neither, it counts as recent (fails closed).
pub fn recent(modified: Option<i64>, added: Option<i64>, now: SystemTime) -> bool {
    let now = match now.duration_since(UNIX_EPOCH) {
        Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        Err(e) => -i64::try_from(e.duration().as_secs()).unwrap_or(i64::MAX),
    };
    let window = RECENT.as_secs() as i64;
    let near = |t: i64| t.abs_diff(now) <= window as u64;
    match (modified, added) {
        (None, None) => true,
        (m, a) => m.is_some_and(near) || a.is_some_and(near),
    }
}

fn app_managing(home: &Path, path: &Path, name: &[u8]) -> Option<&'static str> {
    if let Some((_, app)) = APP_EXTENSIONS.iter().find(|(ext, _)| has_extension(name, ext)) {
        return Some(app);
    }
    let rel = path.strip_prefix(home).ok()?.as_os_str().as_encoded_bytes();
    APP_PATHS.iter().find(|(p, _)| p.as_bytes().eq_ignore_ascii_case(rel)).map(|(_, app)| *app)
}

/// `name` ends with `.ext`, ASCII case-insensitive, and has a stem.
fn has_extension(name: &[u8], ext: &str) -> bool {
    let ext = ext.as_bytes();
    name.len() > ext.len() + 1
        && name[name.len() - ext.len() - 1] == b'.'
        && name[name.len() - ext.len()..].eq_ignore_ascii_case(ext)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const NOW: u64 = 2_000_000_000;

    fn facts<'a>(home: &'a Path, rule: &'a str, path: &'a Path) -> EntryFacts<'a> {
        EntryFacts {
            home,
            rule,
            path,
            modified: Some(NOW as i64 - 86_400),
            added: Some(NOW as i64 - 86_400),
            now: UNIX_EPOCH + Duration::from_secs(NOW),
            kept: false,
            protected: false,
            other_volume: false,
            finished: false,
        }
    }

    fn lock(rule: &str, rel: &str, change: impl FnOnce(&mut EntryFacts<'_>)) -> Option<EntryLock> {
        let home = PathBuf::from("/Users/example");
        let path = home.join(rel);
        let mut f = facts(&home, rule, &path);
        change(&mut f);
        lock_of(&f)
    }

    #[test]
    fn an_old_plain_entry_is_free() {
        assert_eq!(lock("user.downloads", "Downloads/report.pdf", |_| {}), None);
        assert_eq!(lock("user.pictures", "Pictures/Trip", |_| {}), None);
        assert_eq!(
            lock("user.ios-backups", "Library/Application Support/MobileSync/Backup/0000", |f| f.finished = true),
            None
        );
    }

    #[test]
    fn app_libraries_are_locked_by_extension_in_any_case_and_by_fixed_path() {
        let app = |a: &str| Some(EntryLock::AppManaged { app: a.to_owned() });
        assert_eq!(lock("user.pictures", "Pictures/Photos Library.photoslibrary", |_| {}), app("Photos"));
        assert_eq!(lock("user.pictures", "Pictures/Old.PhotosLibrary", |_| {}), app("Photos"));
        assert_eq!(lock("user.pictures", "Pictures/Cat.lrcat-data", |_| {}), app("Lightroom Classic"));
        assert_eq!(lock("user.movies", "Movies/Edit.fcpbundle", |_| {}), app("Final Cut Pro"));
        assert_eq!(lock("user.music", "Music/Music", |_| {}), app("Music"));
        assert_eq!(lock("user.music", "Music/itunes", |_| {}), app("Music"));
        assert_eq!(lock("user.pictures", "Pictures/Lightroom", |_| {}), app("Lightroom Classic"));
        assert_eq!(lock("user.movies", "Movies/Motion Templates.localized", |_| {}), app("Motion"));
        // A name that only ends like an extension, or a bare extension, is not one.
        assert_eq!(lock("user.pictures", "Pictures/notphotoslibrary", |_| {}), None);
        assert_eq!(lock("user.pictures", "Pictures/.photoslibrary", |_| {}), None);
        assert_eq!(lock("user.downloads", "Downloads/Music", |_| {}), None, "fixed paths are home-relative");
    }

    #[test]
    fn partial_downloads_and_unfinished_backups_are_locked() {
        assert_eq!(lock("user.downloads", "Downloads/big.iso.part", |_| {}), Some(EntryLock::PartialDownload));
        assert_eq!(lock("user.downloads", "Downloads/x.CRDOWNLOAD", |_| {}), Some(EntryLock::PartialDownload));
        assert_eq!(lock("user.downloads", "Downloads/x.download", |_| {}), Some(EntryLock::PartialDownload));
        let backup = "Library/Application Support/MobileSync/Backup/0000";
        assert_eq!(lock("user.ios-backups", backup, |_| {}), Some(EntryLock::UnfinishedBackup));
    }

    #[test]
    fn recent_is_ten_minutes_either_side_and_fails_closed_without_times() {
        let at = |m: Option<i64>, a: Option<i64>| {
            lock("user.downloads", "Downloads/f", |f| {
                f.modified = m;
                f.added = a;
            })
        };
        let n = NOW as i64;
        assert_eq!(at(Some(n - 60), Some(n - 86_400)), Some(EntryLock::Recent), "changed a minute ago");
        assert_eq!(at(Some(n - 86_400), Some(n - 60)), Some(EntryLock::Recent), "added a minute ago");
        assert_eq!(at(Some(n + 300), Some(n - 86_400)), Some(EntryLock::Recent), "five minutes ahead");
        assert_eq!(at(Some(n - 601), Some(n - 3_600)), None, "just outside");
        assert_eq!(at(Some(n + 86_400), Some(n - 3_600)), None, "a day ahead: a wrong clock");
        assert_eq!(at(Some(n - 3_600), None), None, "the time it has decides");
        assert_eq!(at(None, Some(n - 30)), Some(EntryLock::Recent));
        assert_eq!(at(None, None), Some(EntryLock::Recent), "neither time: locked");
    }

    #[test]
    fn permanent_reasons_come_before_recent() {
        let fresh = |f: &mut EntryFacts<'_>| f.modified = Some(NOW as i64);
        assert_eq!(
            lock("user.downloads", "Downloads/x", |f| {
                fresh(f);
                f.kept = true;
            }),
            Some(EntryLock::Kept)
        );
        assert_eq!(
            lock("user.pictures", "Pictures/A.photoslibrary", |f| {
                fresh(f);
                f.kept = true;
            }),
            Some(EntryLock::AppManaged { app: "Photos".to_owned() })
        );
        assert_eq!(lock("user.downloads", "Downloads/x", |f| f.other_volume = true), Some(EntryLock::OtherVolume));
        assert_eq!(lock("user.downloads", "Downloads/x", |f| f.protected = true), Some(EntryLock::Protected));
    }

    #[test]
    fn locks_serialize_with_their_reason() {
        let json = serde_json::to_string(&EntryLock::AppManaged { app: "Photos".to_owned() }).unwrap();
        assert_eq!(json, r#"{"reason":"app-managed","app":"Photos"}"#);
        assert_eq!(serde_json::to_string(&EntryLock::OtherVolume).unwrap(), r#"{"reason":"other-volume"}"#);
    }
}
