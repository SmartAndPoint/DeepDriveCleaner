//! Golden outputs (story "Synthetic load trees: large, wide, deep and
//! hostile"): `scan --json` on the S tree of every profile but `hostile`,
//! `disk --json` on `system-root` and `rules --json` once, compared with
//! `bench/golden/`. What differs between runs and machines is normalised
//! first: times, the run's folders, device numbers, the volume's free space.
//! A behaviour change shows up as a difference here; `make golden-record`
//! accepts it.

use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ddc_loadgen::profiles::Profile;
use ddc_loadgen::{Generated, generate, target};
use serde_json::Value;

const S: u64 = 10_000;
const SEED: u64 = 1;

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/golden")
}

/// `DDC_UPDATE_GOLDEN=1` writes the outputs instead of comparing them.
#[allow(clippy::disallowed_methods)] // a switch of the test itself, not of the scanner
fn recording() -> bool {
    std::env::var_os("DDC_UPDATE_GOLDEN").is_some()
}

struct Run {
    _dir: target::Scratch,
    g: Generated,
}

fn build(p: Profile) -> Run {
    let dir = target::Scratch::new().unwrap();
    let g = generate(p, S, SEED, dir.path(), None).unwrap();
    Run { _dir: dir, g }
}

fn ddc(home: &Path, args: &[&str]) -> Value {
    let out = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .env_clear()
        .env("HOME", home)
        .env("PATH", "")
        .env("NO_COLOR", "1")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{args:?}: {e}"))
}

/// The outputs of one profile, normalised, by file name.
fn outputs(run: &Run, p: Profile) -> Vec<(String, Value)> {
    let tree = run.g.layout.tree.to_str().unwrap().to_owned();
    let data = run.g.layout.data.to_str().unwrap().to_owned();
    let home = &run.g.home;
    let mut out = vec![("scan".to_owned(), ddc(home, &["scan", "--json", "--data-dir", &data, &tree]))];
    if p == Profile::SystemRoot {
        out.push(("disk".to_owned(), ddc(home, &["disk", "--json", "--data-dir", &data, "--system-root", &tree])));
    }
    let at = Where {
        base: run.g.layout.tree.parent().unwrap().to_str().unwrap().to_owned(),
        dev: fs::metadata(&run.g.layout.tree).unwrap().dev(),
    };
    for (_, v) in &mut out {
        normalise(v, &at);
    }
    out
}

/// What a run's output names that another run names differently: its
/// folder, and the device the tree is on.
struct Where {
    base: String,
    dev: u64,
}

/// Keys whose value depends on when, where or by which build the run
/// happened: times and ages, inode numbers, the version, the machine's
/// volumes (their mounts, not only their free space) and its Full Disk
/// Access.
const VOLATILE: &[&str] = &[
    "generated_at",
    "duration_ms",
    "as_of",
    "idle_days",
    "ino",
    "parent_ino",
    "ddc_version",
    "full_disk_access",
    "volumes",
    "container",
];

/// The modification time of every `Old` entry ([`ddc_loadgen::desc::OLD_SECS`]);
/// any other activity is the build's own time.
const OLD: &str = "2024-01-01T00:00:00Z";

fn normalise(v: &mut Value, at: &Where) {
    match v {
        Value::String(s) if s.contains(&at.base) => *s = s.replace(&at.base, "<run>"),
        Value::Array(items) => items.iter_mut().for_each(|i| normalise(i, at)),
        Value::Object(map) => {
            for (k, item) in map.iter_mut() {
                match k.as_str() {
                    k if VOLATILE.contains(&k) => *item = Value::Null,
                    "last_activity" if item.as_str().is_some_and(|t| t != OLD) => *item = "<build>".into(),
                    // The number depends on the machine; whether an entry
                    // crosses to another device does not.
                    "dev" | "parent_dev" => {
                        if let Some(d) = item.as_u64() {
                            *item = if d == at.dev { "<tree-dev>" } else { "<other-dev>" }.into();
                        }
                    }
                    // What the volume holds besides the tree.
                    "not_counted" => {
                        normalise(item, at);
                        for key in ["bytes", "data_volume_bytes"] {
                            if let Some(x) = item.get_mut(key) {
                                *x = Value::Null;
                            }
                        }
                    }
                    _ => normalise(item, at),
                }
            }
        }
        _ => {}
    }
}

/// The golden file of `name` for `p`; `names` is keyed by the file
/// system's capabilities, which decide what it builds.
fn golden_path(run: &Run, p: Profile, name: &str) -> PathBuf {
    let key = match run.g.manifest.fs_caps {
        Some(c) => format!(
            "-{}{}{}",
            if c.case_insensitive { "ci" } else { "cs" },
            if c.normalization_insensitive { "ni" } else { "ns" },
            if c.invalid_utf8 { "-bytes" } else { "" }
        ),
        None => String::new(),
    };
    golden_dir().join(p.name()).join(format!("{name}{key}.json"))
}

/// The first difference between two values, as a JSON pointer.
fn first_difference(want: &Value, got: &Value, at: &str) -> Option<String> {
    match (want, got) {
        (Value::Object(a), Value::Object(b)) => {
            let keys: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            keys.into_iter().find_map(|k| match (a.get(k), b.get(k)) {
                (Some(x), Some(y)) => first_difference(x, y, &format!("{at}/{k}")),
                (x, y) => Some(format!("{at}/{k}: {x:?} != {y:?}")),
            })
        }
        (Value::Array(a), Value::Array(b)) => {
            let first =
                a.iter().zip(b).enumerate().find_map(|(i, (x, y))| first_difference(x, y, &format!("{at}/{i}")));
            match first {
                Some(d) if a.len() != b.len() => Some(format!("{at}: {} items != {}; first at {d}", a.len(), b.len())),
                None if a.len() != b.len() => Some(format!(
                    "{at}: {} items != {}; from {}: {}",
                    a.len(),
                    b.len(),
                    a.len().min(b.len()),
                    short(a.get(b.len()).or(b.get(a.len())).unwrap())
                )),
                d => d,
            }
        }
        _ if want == got => None,
        _ => Some(format!("{at}: {} != {}", short(want), short(got))),
    }
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.len() > 200 { format!("{}…", &s[..200]) } else { s }
}

/// Compares (or records) every output of `p`; returns the differences.
fn check(p: Profile) -> Vec<String> {
    compare(&build(p), p)
}

fn compare(run: &Run, p: Profile) -> Vec<String> {
    outputs(run, p)
        .into_iter()
        .filter_map(|(name, got)| {
            against(&golden_path(run, p, &name), &got).map(|d| format!("{} {name}: {d}", p.name()))
        })
        .collect()
}

/// Records `got` at `path`, or returns how it differs from what is there.
fn against(path: &Path, got: &Value) -> Option<String> {
    if recording() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, serde_json::to_string_pretty(got).unwrap() + "\n").unwrap();
        return None;
    }
    match fs::read(path) {
        Ok(bytes) => first_difference(&serde_json::from_slice(&bytes).unwrap(), got, ""),
        Err(e) => Some(format!("{}: {e}; run `make golden-record`", path.display())),
    }
}

fn assert_golden(diffs: &[String]) {
    assert!(diffs.is_empty(), "differs from bench/golden (`make golden-record` accepts it):\n{}", diffs.join("\n"));
}

/// One test per static profile, and the list [`every_static_profile_has_a_golden_test`] checks.
macro_rules! golden {
    ($($test:ident: $p:ident),* $(,)?) => {
        /// Every static profile; `hostile` changes while it is scanned.
        const GOLDEN: &[Profile] = &[$(Profile::$p),*];
        $(
            #[test]
            fn $test() {
                assert_golden(&check(Profile::$p));
            }
        )*
    };
}

golden! {
    golden_projects: Projects,
    golden_wide: Wide,
    golden_wide_root: WideRoot,
    golden_deep: Deep,
    golden_deep_path_max: DeepPathMax,
    golden_users: Users,
    golden_links: Links,
    golden_names: Names,
    golden_system_root: SystemRoot,
    golden_mixed: Mixed,
}

/// The catalog does not depend on the tree: one file for every profile.
#[test]
fn golden_rules() {
    let dir = target::Scratch::new().unwrap();
    let home = dir.path().join("home");
    fs::create_dir(&home).unwrap();
    let mut got = ddc(&home, &["rules", "--json"]);
    let at = Where { base: dir.path().to_str().unwrap().to_owned(), dev: fs::metadata(&home).unwrap().dev() };
    normalise(&mut got, &at);
    let d = against(&golden_dir().join("rules.json"), &got);
    assert_golden(&d.into_iter().map(|d| format!("rules: {d}")).collect::<Vec<_>>());
}

/// The check catches a change to one rule: switched off in `settings.json`,
/// its findings are reported as ignored and leave the plan.
#[test]
fn a_change_to_one_rule_fails_the_golden_check() {
    if recording() {
        return;
    }
    let run = build(Profile::Projects);
    let settings = serde_json::json!({ "schema_version": 1, "disabled_rules": ["python.venv"] });
    fs::write(run.g.layout.data.join("settings.json"), settings.to_string()).unwrap();
    let diffs = compare(&run, Profile::Projects);
    assert!(diffs.iter().any(|d| d.starts_with("projects scan: /findings/")), "{diffs:?}");
}

#[test]
fn every_static_profile_has_a_golden_test() {
    let tested: Vec<_> = GOLDEN.iter().map(|p| p.name()).collect();
    for p in Profile::ALL {
        assert_eq!(tested.contains(&p.name()), p != Profile::Hostile, "{}", p.name());
    }
}
