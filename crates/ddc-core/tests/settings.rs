//! `settings.json`: loading, precedence of flags, missing folders and the
//! excludes an `--exclude` drops (story "CLI: shared settings, types and the
//! notify decision").

mod common;

use ddc_core::platform::Canonical as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::TreeBuilder;
use ddc_core::env::Env;
use ddc_core::settings::{self, ScanFlags, SchedulerOwner, SchedulerRecord, Settings, SettingsError};

struct Fx {
    t: TreeBuilder,
    data: tempfile::TempDir,
}

impl Fx {
    fn new() -> Self {
        Self { t: TreeBuilder::new(), data: tempfile::tempdir().unwrap() }
    }

    fn env(&self) -> Env {
        Env::with_paths(self.t.root(), self.data.path(), &self.data.path().join("logs"), SystemTime::now()).unwrap()
    }

    fn write(&self, json: &str) {
        std::fs::write(self.data.path().join(settings::SETTINGS), json).unwrap();
    }

    fn settings(&self, roots: &[&str], excludes: &[&str]) -> Settings {
        Settings {
            roots: roots.iter().map(|r| self.t.path(r)).collect(),
            excludes: excludes.iter().map(|e| self.t.path(e)).collect(),
            ..Settings::default()
        }
    }
}

#[test]
fn no_file_and_an_empty_roots_list_give_home() {
    let fx = Fx::new();
    let s = settings::load(&fx.env()).unwrap();
    assert_eq!(s, Settings::default());
    let r = s.resolve(&fx.env(), &ScanFlags::default()).unwrap();
    assert!(r.options.roots.is_empty(), "empty roots: the home folder");
    assert_eq!(r.options.min_age, Duration::from_secs(14 * 86_400));
    assert!(r.missing.is_empty() && r.dropped_excludes.is_empty());

    fx.write(r#"{"schema_version": 1, "roots": []}"#);
    let empty = settings::load(&fx.env()).unwrap().resolve(&fx.env(), &ScanFlags::default()).unwrap();
    assert!(empty.options.roots.is_empty(), "an empty list is the home folder, not an error");
}

#[test]
fn the_file_applies_and_a_flag_wins() {
    let fx = Fx::new();
    for d in ["p", "q", "p/keep", "q/keep"] {
        fx.t.dir(d);
    }
    let s = Settings {
        min_age_days: Some(30),
        min_size: Some(50 << 20),
        disabled_rules: vec!["cache.npm".into()],
        ..fx.settings(&["p"], &["p/keep"])
    };

    let file = s.resolve(&fx.env(), &ScanFlags::default()).unwrap().options;
    assert_eq!(file.roots, [fx.t.path("p")]);
    assert_eq!(file.excludes, [fx.t.path("p/keep")]);
    assert_eq!(file.min_age, Duration::from_secs(30 * 86_400));
    assert_eq!((file.min_size, file.disabled_rules.as_slice()), (50 << 20, ["cache.npm".to_owned()].as_slice()));

    let flags = ScanFlags {
        paths: vec![fx.t.path("q")],
        excludes: vec![fx.t.path("q/keep")],
        min_age_days: Some(3),
        ..ScanFlags::default()
    };
    let flagged = s.resolve(&fx.env(), &flags).unwrap().options;
    assert_eq!(flagged.roots, [fx.t.path("q")]);
    assert_eq!(flagged.excludes, [fx.t.path("q/keep")]);
    assert_eq!(flagged.min_age, Duration::from_secs(3 * 86_400));
    assert_eq!(flagged.min_size, 50 << 20, "min_size and disabled_rules come from the file only");
}

#[test]
fn missing_settings_folders_are_skipped_and_all_roots_missing_is_an_error() {
    let fx = Fx::new();
    fx.t.dir("here");
    let s = fx.settings(&["here", "unplugged"], &["gone"]);

    let r = s.resolve(&fx.env(), &ScanFlags::default()).unwrap();
    assert_eq!(r.options.roots, [fx.t.path("here")]);
    assert!(r.options.excludes.is_empty());
    assert_eq!(r.missing, [fx.t.path("unplugged"), fx.t.path("gone")]);

    let none = fx.settings(&["unplugged"], &[]);
    let err = none.resolve(&fx.env(), &ScanFlags::default()).unwrap_err();
    assert!(matches!(&err, SettingsError::NoRoots { missing, .. } if missing == &[fx.t.path("unplugged")]));
    assert!(err.to_string().contains("settings.json"), "{err}");
    let flagged = none.resolve(&fx.env(), &ScanFlags { paths: vec![fx.t.path("here")], ..ScanFlags::default() });
    assert!(flagged.is_ok(), "a flag replaces the missing settings roots");
}

#[test]
fn an_exclude_flag_names_only_the_protected_folders_it_drops() {
    let fx = Fx::new();
    for d in ["r/a/b", "r/c", "elsewhere/x"] {
        fx.t.dir(d);
    }
    if common::skipped_without(
        "an_exclude_flag_names_only_the_protected_folders_it_drops",
        fx.t.try_symlink(fx.t.path("r/c"), "link-to-c"),
    )
    .is_none()
    {
        return;
    }
    let s = fx.settings(&["r"], &["r/a/b", "r/c", "elsewhere/x"]);
    let drop = |flag: &[PathBuf]| -> Vec<PathBuf> {
        s.resolve(&fx.env(), &ScanFlags { excludes: flag.to_vec(), ..ScanFlags::default() }).unwrap().dropped_excludes
    };

    assert_eq!(drop(&[fx.t.path("r/a")]), [fx.t.path("r/c")], "under a flag exclude: still kept; outside: untouched");
    assert!(drop(&[fx.t.path("r/a"), fx.t.path("link-to-c")]).is_empty(), "the same folder by another spelling");
    assert!(
        s.resolve(&fx.env(), &ScanFlags::default()).unwrap().dropped_excludes.is_empty(),
        "no flag: nothing dropped"
    );
}

/// "Skip entirely" (safety ADR, amendment of 2026-10-28): `--skip` adds to the
/// file's skips, a skip is also an exclude no `--exclude` drops, and every
/// skip is a canonical folder.
#[test]
fn skips_add_up_protect_and_must_be_folders() {
    let fx = Fx::new();
    for d in ["r/raw", "r/vm", "r/kept/deep", "r/flag"] {
        fx.t.dir(d);
    }
    fx.t.file("r/notes.txt", 1);
    if common::skipped_without(
        "skips_add_up_protect_and_must_be_folders",
        fx.t.try_symlink(fx.t.path("r/flag"), "link-to-flag"),
    )
    .is_none()
    {
        return;
    }
    let canonical = |rel: &str| fx.t.path(rel).canonical().unwrap();
    let s = Settings {
        roots: vec![fx.t.path("r")],
        excludes: vec![fx.t.path("r/kept/deep")],
        skips: vec![fx.t.path("r/raw"), fx.t.path("r/vm"), fx.t.path("r/unplugged")],
        ..Settings::default()
    };

    let flags = ScanFlags { skips: vec![fx.t.path("link-to-flag"), fx.t.path("r/raw")], ..ScanFlags::default() };
    let resolved = s.resolve(&fx.env(), &flags).unwrap();
    assert_eq!(
        resolved.options.user_skips,
        [canonical("r/raw"), canonical("r/vm"), canonical("r/flag")],
        "the file's, then the flags', canonical, each once"
    );
    assert_eq!(resolved.missing, [fx.t.path("r/unplugged")], "a missing skip is skipped with a note");
    for skip in &resolved.options.user_skips {
        assert!(resolved.options.excludes.contains(skip), "{} protects as an exclude", skip.display());
    }

    // An exclude flag replaces the file's excludes, never its skips, and a
    // folder a skip still covers is not named as dropped.
    let s = Settings { skips: vec![fx.t.path("r/kept")], ..s };
    let flags = ScanFlags { excludes: vec![fx.t.path("r/vm")], ..ScanFlags::default() };
    let resolved = s.resolve(&fx.env(), &flags).unwrap();
    assert!(resolved.options.excludes.contains(&canonical("r/kept")));
    assert!(resolved.dropped_excludes.is_empty(), "{:?}", resolved.dropped_excludes);

    for (skips, flag, origin) in [
        (vec![fx.t.path("r/notes.txt")], vec![], "skips in"),
        (vec![], vec![fx.t.path("r/notes.txt")], "--skip"),
        (vec![], vec![fx.t.path("r/nowhere")], "--skip"),
    ] {
        assert_eq!(settings::SETTINGS_SCHEMA_VERSION, 1, "skips is an optional key");
        let s = Settings { skips, ..Settings::default() };
        let err = s.resolve(&fx.env(), &ScanFlags { skips: flag, ..ScanFlags::default() }).unwrap_err();
        assert!(matches!(err, SettingsError::Skip { .. }), "{err}");
        assert!(
            err.to_string().contains(origin) && err.to_string().contains("notes.txt")
                || err.to_string().contains("nowhere"),
            "{err}"
        );
    }
}

#[test]
fn a_malformed_file_is_an_error_naming_it() {
    let fx = Fx::new();
    for (json, why) in [
        ("{", "EOF"),
        (r#"{"roots": []}"#, "no schema_version"),
        (r#"{"schema_version": 2}"#, "schema_version 2"),
        (r#"{"schema_version": 1, "colour": "blue"}"#, "unknown field"),
        (r#"{"schema_version": 1, "roots": ["Projects"]}"#, "not an absolute path"),
        (r#"{"schema_version": 1, "skips": ["Movies/Raw"]}"#, "not an absolute path"),
        (r#"{"schema_version": 1, "min_age_days": 4000}"#, "min_age_days"),
        (r#"{"schema_version": 1, "min_size": "50M"}"#, "invalid type"),
    ] {
        fx.write(json);
        let err = settings::load(&fx.env()).unwrap_err().to_string();
        assert!(err.contains(&fx.data.path().join("settings.json").display().to_string()), "{err}");
        assert!(err.contains(why), "{json}: {err}");
    }
}

#[test]
fn clearing_the_scheduler_keeps_every_other_field() {
    let fx = Fx::new();
    let s = Settings {
        disabled_rules: vec!["cache.npm".into()],
        notify_threshold: Some(5 << 30),
        scheduler: Some(SchedulerRecord { by: SchedulerOwner::App, app: "/Applications/Gone.app".into() }),
        ..Settings::default()
    };
    fx.write(&serde_json::to_string(&s).unwrap());

    settings::clear_scheduler(&fx.env()).unwrap();

    assert_eq!(settings::load(&fx.env()).unwrap(), Settings { scheduler: None, ..s });
}

#[test]
#[allow(clippy::disallowed_methods)] // DDC_UPDATE_SCHEMA regenerates the committed file
fn committed_settings_schema_is_current() {
    let schema = serde_json::to_string_pretty(&schemars::schema_for!(Settings)).unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/settings.schema.json");
    if std::env::var_os("DDC_UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &schema).unwrap();
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(committed == schema, "docs/settings.schema.json is stale: run DDC_UPDATE_SCHEMA=1 cargo test");
}

/// `agents_clean_safe` (ADR "Agent footprint", §7): read when true, false
/// when absent, and written only when true, so a file stays readable by an
/// older ddc while the opt-in is off.
#[test]
fn the_agents_opt_in_is_written_only_when_on() {
    let on = settings::parse(br#"{"schema_version": 1, "agents_clean_safe": true}"#).unwrap();
    assert!(on.agents_clean_safe);
    assert!(!settings::parse(br#"{"schema_version": 1}"#).unwrap().agents_clean_safe);
    assert!(settings::parse(br#"{"schema_version": 1, "agents_clean_safe": "yes"}"#).is_err());
    let off = serde_json::to_value(Settings::default()).unwrap();
    assert!(off.get("agents_clean_safe").is_none(), "{off}");
    assert_eq!(serde_json::to_value(&on).unwrap()["agents_clean_safe"], true);
}

/// A key this ddc does not know is one a newer DeepDriveCleaner wrote: the
/// refusal says to update.
#[test]
fn an_unknown_key_says_to_update_ddc() {
    let err = settings::parse(br#"{"schema_version": 1, "from_the_future": true}"#).unwrap_err();
    assert!(err.contains("from_the_future") && err.contains("update ddc"), "{err}");
}

// Story "Settings: let coding agents clean safe items": the switch, under
// `settings.lock`, with its journal line and the rollback of `on`.

use ddc_core::exec::{JOURNAL_SCHEMA_VERSION, JournalKind, MemorySettingsJournal};

fn never_opened() -> std::io::Result<MemorySettingsJournal> {
    panic!("the journal is opened only for a change")
}

fn json(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

/// A file with every kind of field and a private mode.
fn full(fx: &Fx) -> PathBuf {
    std::fs::create_dir_all(fx.t.path("p")).unwrap();
    let s = Settings {
        roots: vec![fx.t.path("p")],
        disabled_rules: vec!["node.node_modules".into()],
        min_size: Some(1024),
        notify_low_disk: Some(false),
        scheduler: Some(SchedulerRecord {
            by: SchedulerOwner::App,
            app: PathBuf::from("/Applications/DeepDriveCleaner.app"),
        }),
        ..Settings::default()
    };
    fx.write(&serde_json::to_string_pretty(&s).unwrap());
    let path = fx.data.path().join(settings::SETTINGS);
    private(&path);
    path
}

#[cfg(unix)]
fn private(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}
#[cfg(not(unix))]
fn private(_: &Path) {}

#[cfg(unix)]
fn assert_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    assert_eq!(std::fs::metadata(path).unwrap().permissions().mode() & 0o777, 0o600, "the mode is kept");
}
#[cfg(not(unix))]
fn assert_private(_: &Path) {}

#[test]
fn switching_on_and_off_keeps_every_other_key_and_journals_each_change() {
    let fx = Fx::new();
    let path = full(&fx);
    let before = json(&path);
    let mut journal = MemorySettingsJournal::default();

    let on = settings::set_agents_clean_safe(&fx.env(), true, || Ok(&mut journal)).unwrap();

    assert!(on.changed() && !on.previous && on.now);
    let mut want = before.clone();
    want["agents_clean_safe"] = true.into();
    assert_eq!(json(&path), want, "only the key changed");
    assert_private(&path);
    assert_eq!(journal.lines.len(), 1);
    let line = serde_json::to_value(&journal.lines[0]).unwrap();
    assert_eq!(line["kind"], "settings-set");
    assert_eq!(line["schema_version"], JOURNAL_SCHEMA_VERSION);
    assert_eq!(
        (line["key"].as_str(), line["value"].as_bool(), line["previous"].as_bool()),
        (Some("agents_clean_safe"), Some(true), Some(false))
    );
    assert_eq!(journal.lines[0].kind, JournalKind::SettingsSet);
    assert_eq!(JOURNAL_SCHEMA_VERSION, 14);

    let off = settings::set_agents_clean_safe(&fx.env(), false, || Ok(&mut journal)).unwrap();
    assert!(off.changed() && off.previous && !off.now);
    assert_eq!(json(&path), before, "off removes the key");
    assert_eq!(journal.lines.len(), 2);
    assert_eq!((journal.lines[1].value, journal.lines[1].previous), (false, true));
}

#[test]
fn a_value_already_in_place_opens_and_writes_nothing() {
    let fx = Fx::new();
    let path = fx.data.path().join(settings::SETTINGS);

    let off = settings::set_agents_clean_safe(&fx.env(), false, never_opened).unwrap();

    assert!(!off.changed());
    assert!(!path.exists(), "no settings.json made for nothing");
    assert!(!fx.data.path().join("logs").exists(), "no journal");
    fx.write(r#"{"schema_version": 1, "agents_clean_safe": true}"#);
    let bytes = std::fs::read(&path).unwrap();
    assert!(!settings::set_agents_clean_safe(&fx.env(), true, never_opened).unwrap().changed());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn on_without_a_file_writes_one_with_only_the_version_and_the_key() {
    let fx = Fx::new();
    let mut journal = MemorySettingsJournal::default();

    settings::set_agents_clean_safe(&fx.env(), true, || Ok(&mut journal)).unwrap();

    let path = fx.data.path().join(settings::SETTINGS);
    assert_eq!(json(&path), serde_json::json!({"schema_version": 1, "agents_clean_safe": true}));
    assert!(settings::load(&fx.env()).unwrap().agents_clean_safe);
}

#[test]
fn an_unreadable_file_or_journal_leaves_the_settings_as_they_were() {
    let fx = Fx::new();
    let path = fx.data.path().join(settings::SETTINGS);
    for bad in
        ["{ not json", "null", r#"{"schema_version": 2}"#, r#"{"schema_version": 1, "agents_clean_review": true}"#]
    {
        fx.write(bad);
        let err = settings::set_agents_clean_safe(&fx.env(), true, never_opened).unwrap_err();
        assert!(matches!(err, SettingsError::Malformed { .. }), "{bad}: {err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), bad);
    }
    let path = full(&fx);
    let bytes = std::fs::read(&path).unwrap();
    let err = settings::set_agents_clean_safe(&fx.env(), true, || {
        Err::<MemorySettingsJournal, _>(std::io::Error::other("read-only"))
    })
    .unwrap_err();
    assert!(matches!(err, SettingsError::JournalOpen { .. }), "{err}");
    assert!(err.to_string().contains("nothing changed"), "{err}");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn a_journal_line_that_fails_after_on_puts_the_settings_back() {
    let fx = Fx::new();
    let path = full(&fx);
    let before = json(&path);
    let mut failing = MemorySettingsJournal { fail: true, ..MemorySettingsJournal::default() };

    let err = settings::set_agents_clean_safe(&fx.env(), true, || Ok(&mut failing)).unwrap_err();

    assert!(matches!(err, SettingsError::JournalWrite { on: true, .. }), "{err}");
    assert!(err.to_string().contains("stays off"), "{err}");
    assert_eq!(json(&path), before);
    assert_private(&path);

    // No file before: it reads as the defaults after, and nothing is removed.
    let fx = Fx::new();
    let mut failing = MemorySettingsJournal { fail: true, ..MemorySettingsJournal::default() };
    settings::set_agents_clean_safe(&fx.env(), true, || Ok(&mut failing)).unwrap_err();
    assert_eq!(settings::load(&fx.env()).unwrap(), Settings::default());

    // After off, off stays and the error says the line is missing.
    let fx = Fx::new();
    fx.write(r#"{"schema_version": 1, "agents_clean_safe": true}"#);
    let mut failing = MemorySettingsJournal { fail: true, ..MemorySettingsJournal::default() };
    let err = settings::set_agents_clean_safe(&fx.env(), false, || Ok(&mut failing)).unwrap_err();
    assert!(matches!(err, SettingsError::JournalWrite { on: false, .. }), "{err}");
    assert!(!settings::load(&fx.env()).unwrap().agents_clean_safe);
}

#[test]
fn a_writer_that_holds_the_settings_lock_makes_another_give_up() {
    let fx = Fx::new();
    let path = full(&fx);
    let bytes = std::fs::read(&path).unwrap();
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(fx.data.path().join(settings::SETTINGS_LOCK))
        .unwrap();
    held.try_lock().unwrap();
    let started = std::time::Instant::now();

    let err = settings::set_agents_clean_safe(&fx.env(), true, never_opened).unwrap_err();

    assert!(matches!(err, SettingsError::Busy { .. }), "{err}");
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(900), "it tried again for about a second: {waited:?}");
    assert!(waited < Duration::from_secs(5), "within its bound: {waited:?}");
    assert!(matches!(settings::clear_scheduler(&fx.env()).unwrap_err(), SettingsError::Busy { .. }));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    held.unlock().unwrap();
    drop(held);
    assert!(!settings::clear_scheduler(&fx.env()).unwrap().unlocked);
    assert!(settings::load(&fx.env()).unwrap().scheduler.is_none());
    assert_private(&path);
}

#[test]
fn a_writer_that_lets_go_within_the_bound_lets_the_other_write() {
    let fx = Fx::new();
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(fx.data.path().join(settings::SETTINGS_LOCK))
        .unwrap();
    held.try_lock().unwrap();
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        held.unlock().unwrap();
    });
    let mut journal = MemorySettingsJournal::default();

    let on = settings::set_agents_clean_safe(&fx.env(), true, || Ok(&mut journal)).unwrap();

    release.join().unwrap();
    assert!(on.changed());
    assert!(settings::load(&fx.env()).unwrap().agents_clean_safe);
}

/// A journal that fails its line after it made the data folder read-only,
/// so putting the settings back fails too.
#[cfg(unix)]
struct Sealing<'a>(&'a Path);

#[cfg(unix)]
impl ddc_core::exec::SettingsJournal for Sealing<'_> {
    fn record_setting(&mut self, _: &ddc_core::exec::SettingsSetEntry) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(self.0, std::fs::Permissions::from_mode(0o500))?;
        Err(std::io::Error::other("no space left"))
    }
}

/// Unix only: Windows' folders take no write bits.
#[cfg(unix)]
#[test]
fn a_rollback_that_fails_says_the_opt_in_is_still_on() {
    use std::os::unix::fs::PermissionsExt as _;
    let fx = Fx::new();
    let path = full(&fx);

    let err = settings::set_agents_clean_safe(&fx.env(), true, || Ok(Sealing(fx.data.path()))).unwrap_err();

    std::fs::set_permissions(fx.data.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(matches!(err, SettingsError::RollbackFailed { .. }), "{err}");
    let text = err.to_string();
    assert!(text.contains("still on") && text.contains("switch it off"), "{text}");
    assert_eq!(json(&path)["agents_clean_safe"], true, "it says what is true");
}
