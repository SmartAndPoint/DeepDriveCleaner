//! `<data-dir>/settings.json`: what the app's Settings and the terminal share
//! (ADR "Menu bar app: Swift shell over the ddc CLI", §6 item 6). The app
//! writes it; the CLI reads it and writes it only to clear a stale
//! `scheduler` record and to switch `agents_clean_safe` (`ddc settings
//! set`, amendment of 2026-11-15). Every writer holds `settings.lock`
//! around read, edit and replace. Schema: `docs/settings.schema.json`.

use crate::platform::Canonical as _;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::env::Env;
use crate::model::ExclusionOrigin;
use crate::report::{DEFAULT_MIN_AGE, DEFAULT_WORKTREE_MIN_AGE, ScanOptions};

pub const SETTINGS: &str = "settings.json";
/// The advisory lock every writer of [`SETTINGS`] holds, the app's too.
pub const SETTINGS_LOCK: &str = "settings.lock";
/// How long a writer tries for [`SETTINGS_LOCK`] before it gives up.
const LOCK_WAIT: Duration = Duration::from_secs(1);
const LOCK_RETRY: Duration = Duration::from_millis(20);
/// Version of [`Settings`]; the scan report has its own.
pub const SETTINGS_SCHEMA_VERSION: u32 = 1;
const MAX_MIN_AGE_DAYS: u64 = 3650;

/// Unknown keys are refused: app-only preferences never belong here, and an
/// unknown key may be a protection this CLI does not know about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub schema_version: u32,
    /// Absolute folders to scan; empty means the home folder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<PathBuf>,
    /// Absolute folders never touched ("Never touch these folders"): reported
    /// as `user.kept`, never cleaned.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excludes: Vec<PathBuf>,
    /// Absolute folders never read ("Skip entirely"): not opened, listed in
    /// the report's `not_scanned`, and protected as excludes are. `--skip`
    /// only adds to them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skips: Vec<PathBuf>,
    /// Rule ids switched off in the Types settings: their findings are
    /// `ignored`. Unknown ids are inert, so a renamed rule breaks nothing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_rules: Vec<String>,
    /// `rebuildable` findings idle at least this long are selected by default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_age_days: Option<u64>,
    /// Bytes: smaller findings are `ignored`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_size: Option<u64>,
    /// Bytes: the background scan notifies from this much.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_threshold: Option<u64>,
    /// Also notify when a scanned volume is under 10 % free (default: true).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_low_disk: Option<bool>,
    /// Who runs the scheduled scan (ADR §5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler: Option<SchedulerRecord>,
    /// "Let coding agents clean safe items" (ADR "Agent footprint", §7):
    /// `ddc agent clean` removes `safe` items of the agent's project only
    /// while it is on. Written only when true, so an older ddc refuses the
    /// file only while it is.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub agents_clean_safe: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            roots: Vec::new(),
            excludes: Vec::new(),
            skips: Vec::new(),
            disabled_rules: Vec::new(),
            min_age_days: None,
            min_size: None,
            notify_threshold: None,
            notify_low_disk: None,
            scheduler: None,
            agents_clean_safe: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SchedulerRecord {
    pub by: SchedulerOwner,
    /// The app bundle that registered the scheduler agent.
    pub app: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SchedulerOwner {
    App,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("malformed settings {path}: {reason}")]
    Malformed { path: PathBuf, reason: String },
    #[error("none of the folders to scan in {path} exists: {}", list(.missing))]
    NoRoots { path: PathBuf, missing: Vec<PathBuf> },
    /// A skip that is not a folder, or a `--skip` that cannot be resolved.
    #[error("cannot skip {}: {reason} ({origin})", .skip.display())]
    Skip { skip: PathBuf, origin: String, reason: String },
    #[error("cannot write {path}: {source}; nothing changed")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The switch to `on` was written, its journal line failed, and putting
    /// the settings back failed too: the opt-in is on without its record.
    #[error(
        "agents_clean_safe is still on, and the action journal {journal} has no line for it ({line}); putting it back failed: {source}; switch it off"
    )]
    RollbackFailed {
        path: PathBuf,
        journal: PathBuf,
        line: io::Error,
        #[source]
        source: io::Error,
    },
    /// Another writer held [`SETTINGS_LOCK`] past [`LOCK_WAIT`].
    #[error("another program is changing the settings ({path}); nothing changed, try again")]
    Busy { path: PathBuf },
    #[error("cannot open the action journal {path}: {source}; nothing changed")]
    JournalOpen {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// The switch was written but its journal line was not: `on` was put
    /// back, `off` stays.
    #[error("cannot write the action journal {path}: {source}; {}", journal_kept(*.on))]
    JournalWrite {
        path: PathBuf,
        #[source]
        source: io::Error,
        on: bool,
    },
}

fn journal_kept(on: bool) -> &'static str {
    if on { "agents_clean_safe stays off" } else { "agents_clean_safe is off, but the journal has no line for it" }
}

fn list(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
}

pub fn path(env: &Env) -> PathBuf {
    env.data_dir().join(SETTINGS)
}

/// The settings, or the defaults when there is no file.
pub fn load(env: &Env) -> Result<Settings, SettingsError> {
    let path = path(env);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(source) => return Err(SettingsError::Io { path, source }),
    };
    parse(&bytes).map_err(|reason| SettingsError::Malformed { path, reason })
}

/// Reads `settings.json` from its bytes: the version, the fields, absolute
/// paths and the age limit. The error is the reason, without the path.
pub fn parse(bytes: &[u8]) -> Result<Settings, String> {
    // The version first: a newer file reads as newer, not as unknown fields.
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    match value.get("schema_version").and_then(serde_json::Value::as_u64) {
        Some(v) if v == u64::from(SETTINGS_SCHEMA_VERSION) => {}
        Some(v) => return Err(format!("schema_version {v}; this ddc reads {SETTINGS_SCHEMA_VERSION}")),
        None => return Err("no schema_version".to_owned()),
    }
    let settings: Settings = serde_json::from_value(value).map_err(|e| {
        let reason = e.to_string();
        // A key a newer DeepDriveCleaner wrote (`agents_clean_safe`, say).
        if reason.starts_with("unknown field") {
            format!("{reason}; if a newer DeepDriveCleaner wrote it, update ddc")
        } else {
            reason
        }
    })?;
    if let Some(p) = settings.roots.iter().chain(&settings.excludes).chain(&settings.skips).find(|p| !p.is_absolute()) {
        return Err(format!("{} is not an absolute path", p.display()));
    }
    if settings.min_age_days.is_some_and(|d| d > MAX_MIN_AGE_DAYS) {
        return Err(format!("min_age_days is over {MAX_MIN_AGE_DAYS}"));
    }
    Ok(settings)
}

/// `<data-dir>/settings.lock` while held (menu bar ADR §6 item 6, amendment
/// of 2026-11-15). `unlocked`: the file system takes no locks, and the
/// writer goes on without one, as every writer did before.
struct SettingsLock {
    _file: File,
    unlocked: bool,
}

impl SettingsLock {
    fn take(env: &Env) -> Result<Self, SettingsError> {
        let path = env.data_dir().join(SETTINGS_LOCK);
        let io_err = |source| SettingsError::Write { path: path.clone(), source };
        fs::create_dir_all(env.data_dir()).map_err(io_err)?;
        let file = OpenOptions::new().create(true).truncate(false).write(true).open(&path).map_err(io_err)?;
        let started = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file, unlocked: false }),
                Err(fs::TryLockError::WouldBlock) if started.elapsed() < LOCK_WAIT => std::thread::sleep(LOCK_RETRY),
                Err(fs::TryLockError::WouldBlock) => return Err(SettingsError::Busy { path: path.clone() }),
                Err(fs::TryLockError::Error(e))
                    if e.kind() == io::ErrorKind::Unsupported
                        || e.raw_os_error().is_some_and(crate::platform::errno::lock_unsupported) =>
                {
                    return Ok(Self { _file: file, unlocked: true });
                }
                Err(fs::TryLockError::Error(e)) => return Err(io_err(e)),
            }
        }
    }
}

fn write(env: &Env, settings: &Settings) -> Result<(), SettingsError> {
    let path = path(env);
    let mut json = serde_json::to_vec_pretty(settings).expect("settings serialize");
    json.push(b'\n');
    crate::store::write_atomic(&path, &json).map(drop).map_err(|source| SettingsError::Write { path, source })
}

/// What [`edit`] gave back, and whether it wrote without [`SETTINGS_LOCK`]
/// because the file system takes no locks (the caller says so).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Edited<R> {
    pub value: R,
    pub unlocked: bool,
}

/// Reads the settings under [`SETTINGS_LOCK`], lets `f` change them, and
/// writes them back, keeping the file's mode, only when they changed.
pub fn edit<R>(env: &Env, f: impl FnOnce(&mut Settings) -> R) -> Result<Edited<R>, SettingsError> {
    let lock = SettingsLock::take(env)?;
    let before = load(env)?;
    let mut after = before.clone();
    let value = f(&mut after);
    if after != before {
        write(env, &after)?;
    }
    Ok(Edited { value, unlocked: lock.unlocked })
}

/// Clears the `scheduler` record, keeping every other field.
pub fn clear_scheduler(env: &Env) -> Result<Edited<()>, SettingsError> {
    edit(env, |s| s.scheduler = None)
}

/// What [`set_agents_clean_safe`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Switched {
    pub previous: bool,
    pub now: bool,
    /// The file system took no lock: written without one.
    pub unlocked: bool,
}

impl Switched {
    pub fn changed(&self) -> bool {
        self.previous != self.now
    }
}

/// Switches `agents_clean_safe` (story "Settings: let coding agents clean
/// safe items", D3): under [`SETTINGS_LOCK`], a value already in place
/// opens and writes nothing; otherwise `open` gives the journal, the file
/// is written, and one `settings-set` line follows. If that line fails
/// after `on`, the settings before are written back under the same hold
/// (an absent file then reads as the defaults), so the opt-in is never on
/// without its record.
pub fn set_agents_clean_safe<J: crate::exec::SettingsJournal>(
    env: &Env,
    on: bool,
    open: impl FnOnce() -> io::Result<J>,
) -> Result<Switched, SettingsError> {
    let lock = SettingsLock::take(env)?;
    let before = load(env)?;
    let previous = before.agents_clean_safe;
    let done = Switched { previous, now: on, unlocked: lock.unlocked };
    if previous == on {
        return Ok(done);
    }
    let journal_path = crate::exec::FileJournal::path_in(env);
    let mut journal = open().map_err(|source| SettingsError::JournalOpen { path: journal_path.clone(), source })?;
    let after = Settings { agents_clean_safe: on, ..before.clone() };
    write(env, &after)?;
    let entry = crate::exec::SettingsSetEntry::new("agents_clean_safe", on, previous);
    if let Err(line) = journal.record_setting(&entry) {
        if on && let Err(back) = write(env, &before) {
            let source = match back {
                SettingsError::Write { source, .. } => source,
                other => io::Error::other(other.to_string()),
            };
            return Err(SettingsError::RollbackFailed { path: path(env), journal: journal_path, line, source });
        }
        return Err(SettingsError::JournalWrite { path: journal_path, source: line, on });
    }
    Ok(done)
}

/// What the command line gave; an empty or absent value leaves the setting.
#[derive(Debug, Clone, Default)]
pub struct ScanFlags {
    pub paths: Vec<PathBuf>,
    pub excludes: Vec<PathBuf>,
    /// Added to the settings' skips, never in place of them.
    pub skips: Vec<PathBuf>,
    pub min_age_days: Option<u64>,
    /// `--worktree-min-age`: a harness's worktree idle this many days may
    /// be selected; [`DEFAULT_WORKTREE_MIN_AGE`] without it.
    pub worktree_min_age_days: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub options: ScanOptions,
    /// Settings roots, excludes and skips that do not exist (an unplugged disk): skipped.
    pub missing: Vec<PathBuf>,
    /// Settings excludes an `--exclude` drops while they still protect a
    /// scanned folder: the caller names them before going on.
    pub dropped_excludes: Vec<PathBuf>,
}

impl Settings {
    /// The scan options of a command: a flag wins over its setting;
    /// `disabled_rules` and `min_size` come from the file only.
    pub fn resolve(&self, env: &Env, flags: &ScanFlags) -> Result<Resolved, SettingsError> {
        let mut missing = Vec::new();
        let roots = if !flags.paths.is_empty() {
            // Flags stay strict: the scan fails on a path that is not there.
            flags.paths.clone()
        } else {
            let (found, gone) = existing(&self.roots);
            if !self.roots.is_empty() && found.is_empty() {
                return Err(SettingsError::NoRoots { path: path(env), missing: gone });
            }
            missing.extend(gone);
            found
        };
        let (excludes, dropped_excludes) = if flags.excludes.is_empty() {
            let (found, gone) = existing(&self.excludes);
            missing.extend(gone);
            (found, Vec::new())
        } else {
            let effective = if roots.is_empty() { vec![env.home().to_path_buf()] } else { roots.clone() };
            (flags.excludes.clone(), self.dropped(&flags.excludes, &effective))
        };
        let (found, gone) = existing(&self.skips);
        missing.extend(gone);
        let origin = path(env).display().to_string();
        let mut user_skips = Vec::new();
        // The skips only `--skip` gives: one the file also holds is the file's.
        let mut flag_skips = Vec::new();
        for (skip, origin, flag) in found
            .iter()
            .map(|s| (s, format!("skips in {origin}"), false))
            .chain(flags.skips.iter().map(|s| (s, "--skip".to_owned(), true)))
        {
            let canonical = skip.canonical().map_err(|e| SettingsError::Skip {
                skip: skip.clone(),
                origin: origin.clone(),
                reason: e.to_string(),
            })?;
            if !canonical.is_dir() {
                return Err(SettingsError::Skip { skip: skip.clone(), origin, reason: "not a folder".to_owned() });
            }
            if !user_skips.contains(&canonical) {
                if flag {
                    flag_skips.push(canonical.clone());
                }
                user_skips.push(canonical);
            }
        }
        // After the `--exclude` branch: an exclude flag never drops a skip.
        let mut excludes = excludes;
        excludes.extend(user_skips.iter().cloned());
        let dropped_excludes = dropped_excludes
            .into_iter()
            .filter(|e| e.canonical().map_or(true, |c| !user_skips.iter().any(|s| c.starts_with(s))))
            .collect();
        let min_age_days = flags.min_age_days.or(self.min_age_days);
        Ok(Resolved {
            options: ScanOptions {
                roots,
                excludes,
                user_skips,
                flag_skips,
                excludes_origin: if flags.excludes.is_empty() {
                    ExclusionOrigin::Settings
                } else {
                    ExclusionOrigin::Flag
                },
                min_age: min_age_days.map_or(DEFAULT_MIN_AGE, |d| Duration::from_secs(d * 86_400)),
                worktree_min_age: flags
                    .worktree_min_age_days
                    .map_or(DEFAULT_WORKTREE_MIN_AGE, |d| Duration::from_secs(d * 86_400)),
                disabled_rules: self.disabled_rules.clone(),
                min_size: self.min_size.unwrap_or(0),
                ..ScanOptions::default()
            },
            missing,
            dropped_excludes,
        })
    }

    /// Settings excludes that exist, are neither equal to nor under a flag
    /// exclude (the walk keeps everything under an exclude), and touch a
    /// scanned folder. Compared canonically, as the scan compares excludes.
    fn dropped(&self, flag_excludes: &[PathBuf], roots: &[PathBuf]) -> Vec<PathBuf> {
        let flags: Vec<PathBuf> = flag_excludes.iter().filter_map(|p| p.canonical().ok()).collect();
        let roots: Vec<PathBuf> = roots.iter().filter_map(|p| p.canonical().ok()).collect();
        self.excludes
            .iter()
            .filter(|e| match e.canonical() {
                Ok(c) => {
                    !flags.iter().any(|f| c.starts_with(f))
                        && roots.iter().any(|r| c.starts_with(r) || r.starts_with(&c))
                }
                Err(_) => false,
            })
            .cloned()
            .collect()
    }
}

fn existing(paths: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    paths.iter().cloned().partition(|p| Path::new(p).exists())
}
