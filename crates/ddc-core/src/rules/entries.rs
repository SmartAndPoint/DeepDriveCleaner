//! Entries of a finding's folder: which rules list their direct children,
//! what one entry is, and what locks one. The user's own folders (story
//! "User data: pick your own files for the Trash"; safety ADR, amendment of
//! 2026-10-20) and, since the story "Harness data rules" (epic DDC-003), the
//! coding agents' session folders (ADR "Agent footprint: worktrees, harness
//! data and bounded consent for agents", §3). [`lock_of`] is the one
//! derivation the scan, the dry run and the executor's guard share.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::agents::liveness::{LiveBy, SessionLiveness, UnknownCause};

/// The user's own folders, whose findings list their direct children as
/// entries. iMazing backups stay report only until their layout is checked.
/// Only these: a call site that means any entry rule asks [`policy`].
pub const ENTRY_RULES: &[&str] = &["user.downloads", "user.pictures", "user.movies", "user.music", "user.ios-backups"];

/// The coding agents' session folders, whose sessions are their entries,
/// to the Trash one by one (ADR "Agent footprint", §3). Not [`ENTRY_RULES`].
pub const SESSION_ENTRY_RULES: &[&str] = &["agent.claude-code-file-history", "agent.claude-code-transcripts"];

/// What one entry of a folder is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryShape {
    /// Every direct child is an entry of its own (the user's own folders).
    Child,
    /// A session's transcript, `<session>.jsonl`, keys the entry. Its
    /// companions are the siblings named `<session>` or `<session>.<any>`.
    Transcript,
    /// A session's folder, `<session>/`, keys the entry. Its companions
    /// are the siblings named `<session>.<any>`.
    SessionDir,
}

/// What tells whether an entry is still in use, besides its lock reasons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryLiveness {
    /// Its times: a change within [`RECENT`] locks it `recent`.
    Times,
    /// Claude Code's session check (`agents::liveness::claude`): it locks
    /// the entry `live-session` or `liveness-unknown`, in place of `recent`.
    ClaudeSession,
}

/// How a rule's findings list their entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryPolicy {
    pub shape: EntryShape,
    /// Only entries of at least `ContentsLimits::min_bytes` are listed.
    pub floor: bool,
    pub liveness: EntryLiveness,
    /// The user's own folder: one of [`ENTRY_RULES`].
    pub user_data: bool,
}

const USER_DATA: EntryPolicy =
    EntryPolicy { shape: EntryShape::Child, floor: true, liveness: EntryLiveness::Times, user_data: true };

const SESSIONS: EntryPolicy = EntryPolicy {
    shape: EntryShape::Transcript,
    floor: false,
    liveness: EntryLiveness::ClaudeSession,
    user_data: false,
};

const POLICIES: &[(&str, EntryPolicy)] = &[
    ("user.downloads", USER_DATA),
    ("user.pictures", USER_DATA),
    ("user.movies", USER_DATA),
    ("user.music", USER_DATA),
    ("user.ios-backups", USER_DATA),
    ("agent.claude-code-file-history", EntryPolicy { shape: EntryShape::SessionDir, ..SESSIONS }),
    ("agent.claude-code-transcripts", SESSIONS),
];

/// The entry policy of `rule`, `None` when its findings list no entries.
pub fn policy(rule: &str) -> Option<&'static EntryPolicy> {
    POLICIES.iter().find(|(id, _)| *id == rule).map(|(_, p)| p)
}

/// Whether `rule`'s findings have entries a person can pick: the user's
/// own folders and the coding agents' session folders.
pub fn has_entries(rule: &str) -> bool {
    policy(rule).is_some()
}

/// The session id a child's name starts with: a UUID (`8-4-4-4-12` ASCII
/// hex digits), alone or followed by a dot. A name that does not start with
/// one is never part of a session entry.
pub fn session_of(name: &[u8]) -> Option<&str> {
    const LEN: usize = 36;
    let id = name.get(..LEN)?;
    if name.len() > LEN && name[LEN] != b'.' {
        return None;
    }
    let ok = id
        .iter()
        .enumerate()
        .all(|(i, b)| if matches!(i, 8 | 13 | 18 | 23) { *b == b'-' } else { b.is_ascii_hexdigit() });
    if ok { std::str::from_utf8(id).ok() } else { None }
}

/// What a child of a session folder is to the sessions, by its name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPart<'a> {
    /// It keys `session`'s entry.
    Key(&'a str),
    /// A companion of `session`'s entry, when that entry exists.
    Companion(&'a str),
    /// No part of any session.
    None,
}

/// What `name` is in a folder of `shape`; `is_dir` is the child's kind. A
/// key of the wrong kind (a folder named `<session>.jsonl`, a file named
/// `<session>` in `file-history`) is no part.
pub fn session_part(shape: EntryShape, name: &[u8], is_dir: bool) -> SessionPart<'_> {
    let Some(session) = session_of(name) else { return SessionPart::None };
    let rest = &name[session.len()..];
    match shape {
        EntryShape::Child => SessionPart::None,
        EntryShape::Transcript if rest == b".jsonl" => {
            if is_dir {
                SessionPart::None
            } else {
                SessionPart::Key(session)
            }
        }
        EntryShape::Transcript => SessionPart::Companion(session),
        EntryShape::SessionDir if rest.is_empty() => {
            if is_dir {
                SessionPart::Key(session)
            } else {
                SessionPart::None
            }
        }
        EntryShape::SessionDir => SessionPart::Companion(session),
    }
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
    /// A coding agent's session that is live: a live record names it, a
    /// file of it changed within [`RECENT`], or a process holds one open.
    /// In place of `recent` for the session folders.
    LiveSession { session: String, by: LiveBy },
    /// Whether the session is live could not be told: the harness's live
    /// records could not be read or decided.
    LivenessUnknown { cause: UnknownCause },
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
    /// For an entry of a session folder ([`EntryLiveness::ClaudeSession`]):
    /// its session's state. `None` there counts as unknown (fails closed);
    /// no other rule reads it.
    pub session: Option<SessionLiveness>,
}

/// The entry's lock, permanent reasons first and "recent", which clears by
/// itself, last. A session folder's entry takes its session's liveness in
/// place of "recent".
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
    if policy(f.rule).is_some_and(|p| p.liveness == EntryLiveness::ClaudeSession) {
        return session_lock(name, f.session);
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

/// A session entry's lock from its session's state; the session is the
/// one its name starts with.
fn session_lock(name: &[u8], state: Option<SessionLiveness>) -> Option<EntryLock> {
    match state {
        Some(SessionLiveness::NotLive) => None,
        Some(SessionLiveness::Live { by }) => {
            let session = session_of(name).map_or_else(|| String::from_utf8_lossy(name).into_owned(), str::to_owned);
            Some(EntryLock::LiveSession { session, by })
        }
        Some(SessionLiveness::Unknown { cause }) => Some(EntryLock::LivenessUnknown { cause }),
        None => Some(EntryLock::LivenessUnknown { cause: UnknownCause::SessionWithoutFiles }),
    }
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
            session: None,
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
        let live = EntryLock::LiveSession { session: S.to_owned(), by: LiveBy::OpenFile };
        assert_eq!(
            serde_json::to_string(&live).unwrap(),
            format!(r#"{{"reason":"live-session","session":"{S}","by":"open-file"}}"#)
        );
        let unknown = EntryLock::LivenessUnknown { cause: UnknownCause::LiveRecordWithoutSession };
        assert_eq!(
            serde_json::to_string(&unknown).unwrap(),
            r#"{"reason":"liveness-unknown","cause":"live-record-without-session"}"#
        );
    }

    /// A fixture session id, never a real one.
    const S: &str = "0a1b2c3d-0000-4000-8000-00000000000a";
    const TRANSCRIPTS: &str = "agent.claude-code-transcripts";
    const FILE_HISTORY: &str = "agent.claude-code-file-history";

    #[test]
    fn entry_rules_are_the_user_data_policies_and_the_sessions_have_their_own() {
        let user: Vec<_> = POLICIES.iter().filter(|(_, p)| p.user_data).map(|(id, _)| *id).collect();
        assert_eq!(user, ENTRY_RULES, "ENTRY_RULES keeps meaning the user's own folders");
        let sessions: Vec<_> = POLICIES.iter().filter(|(_, p)| !p.user_data).map(|(id, _)| *id).collect();
        assert_eq!(sessions.len(), SESSION_ENTRY_RULES.len());
        assert!(sessions.iter().all(|id| SESSION_ENTRY_RULES.contains(id)));
        for id in ENTRY_RULES {
            let p = policy(id).unwrap();
            assert_eq!((p.shape, p.floor, p.liveness), (EntryShape::Child, true, EntryLiveness::Times), "{id}");
        }
        let t = policy(TRANSCRIPTS).unwrap();
        assert_eq!((t.shape, t.floor, t.liveness), (EntryShape::Transcript, false, EntryLiveness::ClaudeSession));
        let h = policy(FILE_HISTORY).unwrap();
        assert_eq!((h.shape, h.floor, h.liveness), (EntryShape::SessionDir, false, EntryLiveness::ClaudeSession));
        assert!(has_entries(TRANSCRIPTS) && has_entries("user.downloads"));
        assert!(!has_entries("agent.claude-code-history") && !has_entries("agent.codex-sessions"));
        assert!(policy("node.node_modules").is_none());
    }

    #[test]
    fn a_session_id_is_a_uuid_alone_or_before_a_dot() {
        assert_eq!(session_of(S.as_bytes()), Some(S));
        assert_eq!(session_of(format!("{S}.jsonl").as_bytes()), Some(S));
        assert_eq!(session_of(format!("{S}.desktop-released.json").as_bytes()), Some(S));
        assert_eq!(session_of(S.to_uppercase().as_bytes()), Some(S.to_uppercase().as_str()));
        assert_eq!(session_of(format!("{S}x").as_bytes()), None, "a longer name");
        assert_eq!(session_of(&S.as_bytes()[..35]), None, "a shorter one");
        assert_eq!(session_of(S.replace('-', "_").as_bytes()), None);
        assert_eq!(session_of(S.replacen('a', "g", 1).as_bytes()), None, "not hex");
        assert_eq!(session_of(b"sessions-index.json"), None);
        assert_eq!(session_of(b"memory"), None);
    }

    #[test]
    fn a_transcript_keys_its_session_and_its_siblings_are_companions() {
        use EntryShape::{Child, SessionDir, Transcript};
        use SessionPart::{Companion, Key};
        let jsonl = format!("{S}.jsonl");
        let released = format!("{S}.desktop-released.json");
        fn part(shape: EntryShape, name: &str, dir: bool) -> SessionPart<'_> {
            session_part(shape, name.as_bytes(), dir)
        }
        assert_eq!(part(Transcript, &jsonl, false), Key(S));
        assert_eq!(part(Transcript, &jsonl, true), SessionPart::None, "a folder is no key");
        assert_eq!(part(Transcript, S, true), Companion(S));
        assert_eq!(part(Transcript, &released, false), Companion(S));
        assert_eq!(part(Transcript, "memory", true), SessionPart::None);
        // `file-history`: the session's folder is the key.
        assert_eq!(part(SessionDir, S, true), Key(S));
        assert_eq!(part(SessionDir, S, false), SessionPart::None, "a file is no key");
        assert_eq!(part(SessionDir, &jsonl, false), Companion(S));
        assert_eq!(part(Child, &jsonl, false), SessionPart::None);
    }

    #[test]
    fn a_session_entry_takes_its_liveness_in_place_of_recent() {
        let entry = format!(".claude/projects/-Users-example-p/{S}.jsonl");
        let at = |state: Option<SessionLiveness>, change: fn(&mut EntryFacts<'_>)| {
            lock(TRANSCRIPTS, &entry, |f| {
                f.session = state;
                change(f);
            })
        };
        let fresh = |f: &mut EntryFacts<'_>| {
            f.modified = Some(NOW as i64);
            f.added = Some(NOW as i64);
        };
        assert_eq!(at(Some(SessionLiveness::NotLive), |_| {}), None);
        assert_eq!(at(Some(SessionLiveness::NotLive), fresh), None, "no `recent`: liveness decides the times");
        assert_eq!(
            at(Some(SessionLiveness::Live { by: LiveBy::Record }), |_| {}),
            Some(EntryLock::LiveSession { session: S.to_owned(), by: LiveBy::Record })
        );
        let unknown = SessionLiveness::Unknown { cause: UnknownCause::UnparsableRecord };
        assert_eq!(
            at(Some(unknown), |_| {}),
            Some(EntryLock::LivenessUnknown { cause: UnknownCause::UnparsableRecord })
        );
        assert_eq!(
            at(None, |_| {}),
            Some(EntryLock::LivenessUnknown { cause: UnknownCause::SessionWithoutFiles }),
            "no state: fails closed"
        );
        // The permanent reasons still come first.
        let live = Some(SessionLiveness::Live { by: LiveBy::Recent });
        assert_eq!(at(live, |f| f.kept = true), Some(EntryLock::Kept));
        assert_eq!(at(live, |f| f.protected = true), Some(EntryLock::Protected));
        assert_eq!(at(live, |f| f.other_volume = true), Some(EntryLock::OtherVolume));
        // `file-history`'s folder too; a user folder never reads the session.
        let dir = format!(".claude/file-history/{S}");
        assert_eq!(
            lock(FILE_HISTORY, &dir, |f| f.session = live),
            Some(EntryLock::LiveSession { session: S.to_owned(), by: LiveBy::Recent })
        );
        assert_eq!(lock("user.downloads", "Downloads/x", |f| f.session = live), None);
    }
}
