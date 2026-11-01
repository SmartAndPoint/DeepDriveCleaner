//! `<data-dir>/settings.json`: what the app's Settings and the terminal share
//! (ADR "Menu bar app: Swift shell over the ddc CLI", §6 item 6). The app
//! writes it; the CLI reads it and writes it only to clear a stale
//! `scheduler` record. Schema: `docs/settings.schema.json`.

use crate::platform::Canonical as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::env::Env;
use crate::model::ExclusionOrigin;
use crate::report::{DEFAULT_MIN_AGE, ScanOptions};

pub const SETTINGS: &str = "settings.json";
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
    let settings: Settings = serde_json::from_value(value).map_err(|e| e.to_string())?;
    if let Some(p) = settings.roots.iter().chain(&settings.excludes).chain(&settings.skips).find(|p| !p.is_absolute()) {
        return Err(format!("{} is not an absolute path", p.display()));
    }
    if settings.min_age_days.is_some_and(|d| d > MAX_MIN_AGE_DAYS) {
        return Err(format!("min_age_days is over {MAX_MIN_AGE_DAYS}"));
    }
    Ok(settings)
}

/// Clears the `scheduler` record, keeping every other field: the CLI's only write.
pub fn clear_scheduler(env: &Env) -> Result<(), SettingsError> {
    let mut settings = load(env)?;
    settings.scheduler = None;
    let path = path(env);
    let mut json = serde_json::to_vec_pretty(&settings).expect("settings serialize");
    json.push(b'\n');
    crate::store::write_atomic(&path, &json).map(drop).map_err(|source| SettingsError::Io { path, source })
}

/// What the command line gave; an empty or absent value leaves the setting.
#[derive(Debug, Clone, Default)]
pub struct ScanFlags {
    pub paths: Vec<PathBuf>,
    pub excludes: Vec<PathBuf>,
    /// Added to the settings' skips, never in place of them.
    pub skips: Vec<PathBuf>,
    pub min_age_days: Option<u64>,
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
