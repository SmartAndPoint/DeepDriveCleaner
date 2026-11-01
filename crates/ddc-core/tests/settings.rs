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
