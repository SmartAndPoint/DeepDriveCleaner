//! Story "Protected by you: say what it is, and Remove Protection": what the
//! scan report says keeps each `user.kept` finding. Temporary homes only.

mod common;

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use common::TreeBuilder;
use ddc_core::env::Env;
use ddc_core::model::{Exclusion, ExclusionKind, ExclusionOrigin, Finding, MarkerFolder, Protection, ScanReport};
use ddc_core::report;
use ddc_core::scan::Progress;
use ddc_core::settings::{ScanFlags, Settings};

struct Home {
    t: TreeBuilder,
    data: tempfile::TempDir,
}

impl Home {
    /// Node projects `a` to `g`, each with `node_modules/dep`, and uv's cache.
    fn new() -> Self {
        let t = TreeBuilder::new();
        for p in ["a", "b", "c", "d", "e", "f", "g"] {
            t.write(&format!("{p}/package.json"), "{}");
            t.write(&format!("{p}/package-lock.json"), "{}");
            t.file(&format!("{p}/node_modules/dep/index.js"), 100);
        }
        t.file(".cache/uv/wheels/a.whl", 100);
        Self { t, data: tempfile::tempdir().unwrap() }
    }

    fn env(&self) -> Env {
        Env::with_paths(self.t.root(), self.data.path(), &self.data.path().join("logs"), SystemTime::now()).unwrap()
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.t.path(rel)
    }

    fn paths(&self, rels: &[&str]) -> Vec<PathBuf> {
        rels.iter().map(|r| self.path(r)).collect()
    }

    /// A scan of the home folder as `ddc scan` runs it: the settings, then the flags.
    fn scan(&self, settings: &Settings, flags: &ScanFlags) -> ScanReport {
        let env = self.env();
        let resolved = settings.resolve(&env, flags).unwrap();
        report::scan(&env, &resolved.options, &Progress::default()).unwrap()
    }

    fn marker(&self, rel: &str) -> MarkerFolder {
        let (dev, ino) = common::id(&self.path(rel));
        MarkerFolder { folder: self.path(rel), dev, ino }
    }

    fn exclusion(&self, rel: &str, kind: ExclusionKind, origin: ExclusionOrigin, inside: bool) -> Exclusion {
        Exclusion { path: self.path(rel), kind, origin, inside }
    }
}

fn finding<'a>(r: &'a ScanReport, h: &Home, rel: &str) -> &'a Finding {
    let path = h.path(rel);
    r.findings.iter().find(|f| f.path == path).unwrap_or_else(|| panic!("no finding at {rel}"))
}

/// The protection of the `user.kept` finding at `rel`.
fn protection<'a>(r: &'a ScanReport, h: &Home, rel: &str) -> &'a Protection {
    let f = finding(r, h, rel);
    assert_eq!(f.rule, "user.kept", "{rel}");
    f.protection.as_ref().unwrap_or_else(|| panic!("{rel} has no protection"))
}

#[test]
fn a_kept_finding_names_what_protects_it_and_the_rule_it_displaced() {
    if common::unsupported("a_kept_finding_names_what_protects_it_and_the_rule_it_displaced") {
        return;
    }
    use ExclusionKind::{Exclude, Skip};
    use ExclusionOrigin::{Flag, Settings as InSettings};
    let h = Home::new();
    h.t.write("a/node_modules/.ddc-keep", "");
    h.t.write("b/node_modules/dep/.ddc-keep", "");
    let settings = Settings {
        excludes: h.paths(&["c/node_modules", "f/node_modules/dep", ".cache/uv"]),
        skips: h.paths(&["d/node_modules/dep"]),
        ..Settings::default()
    };
    // A skip the file also holds is the file's.
    let flags = ScanFlags { skips: h.paths(&["e/node_modules/dep", "d/node_modules/dep"]), ..ScanFlags::default() };
    let r = h.scan(&settings, &flags);

    let marked = protection(&r, &h, "a/node_modules");
    assert_eq!(marked.markers, [h.marker("a/node_modules")], "the marker at the top, with its folder's identity");
    assert!(marked.exclusions.is_empty());
    assert_eq!(marked.rule.as_deref(), Some("node.node_modules"), "the matchers ran before the marker kept it");

    let deep = protection(&r, &h, "b/node_modules");
    assert_eq!(deep.markers, [h.marker("b/node_modules/dep")], "a deeper marker names its own folder");
    assert_eq!(deep.rule.as_deref(), Some("node.node_modules"));

    let excluded = protection(&r, &h, "c/node_modules");
    assert_eq!(excluded.exclusions, [h.exclusion("c/node_modules", Exclude, InSettings, false)]);
    assert!(excluded.markers.is_empty());
    assert_eq!(excluded.rule, None, "nothing under an exclusion is listed: a matcher rule is not known");

    let held_skip = protection(&r, &h, "d/node_modules");
    assert_eq!(held_skip.exclusions, [h.exclusion("d/node_modules/dep", Skip, InSettings, true)]);
    assert_eq!(held_skip.rule.as_deref(), Some("node.node_modules"));

    let held_flag_skip = protection(&r, &h, "e/node_modules");
    assert_eq!(held_flag_skip.exclusions, [h.exclusion("e/node_modules/dep", Skip, Flag, true)]);
    assert_eq!(held_flag_skip.rule.as_deref(), Some("node.node_modules"));

    let held_exclude = protection(&r, &h, "f/node_modules");
    assert_eq!(held_exclude.exclusions, [h.exclusion("f/node_modules/dep", Exclude, InSettings, true)]);
    assert_eq!(held_exclude.rule.as_deref(), Some("node.node_modules"));

    let known = protection(&r, &h, ".cache/uv");
    assert_eq!(known.exclusions, [h.exclusion(".cache/uv", Exclude, InSettings, false)]);
    assert_eq!(known.rule.as_deref(), Some("cache.uv"), "a known path's rule is known without a listing");

    let free = finding(&r, &h, "g/node_modules");
    assert_eq!((free.rule.as_str(), &free.protection), ("node.node_modules", &None), "only kept findings");
    let json = serde_json::to_value(free).unwrap();
    assert!(json.get("protection").is_none(), "absent, not null: {json}");

    let flagged =
        h.scan(&Settings::default(), &ScanFlags { excludes: h.paths(&["c/node_modules"]), ..ScanFlags::default() });
    let by_flag = protection(&flagged, &h, "c/node_modules");
    assert_eq!(by_flag.exclusions, [h.exclusion("c/node_modules", Exclude, Flag, false)]);
}

/// The identities `ddc keep remove` compares: a marker at a finding's top
/// names that folder's own `(dev, ino)`, whichever route claimed it.
#[test]
fn a_marker_at_the_top_names_its_folders_own_identity_on_every_route() {
    if common::unsupported("a_marker_at_the_top_names_its_folders_own_identity_on_every_route") {
        return;
    }
    use ExclusionKind::Exclude;
    use ExclusionOrigin::Settings as InSettings;
    let h = Home::new();
    // A folder named `.ddc-keep`, seen after the read (a matcher rule).
    h.t.dir("a/node_modules/.ddc-keep");
    // A file at a known path's top, seen before the read (`claim_unless_kept`).
    h.t.write(".cache/uv/.ddc-keep", "");
    // Excluded folders that hold a marker at their top: a file and a folder.
    h.t.write("c/node_modules/.ddc-keep", "");
    h.t.dir("d/node_modules/.ddc-keep");
    let settings = Settings { excludes: h.paths(&["c/node_modules", "d/node_modules"]), ..Settings::default() };
    let r = h.scan(&settings, &ScanFlags::default());

    let folder = protection(&r, &h, "a/node_modules");
    assert_eq!(folder.markers, [h.marker("a/node_modules")], "a folder marker names the finding itself");
    assert_eq!(folder.rule.as_deref(), Some("node.node_modules"));

    let known = protection(&r, &h, ".cache/uv");
    assert_eq!(known.markers, [h.marker(".cache/uv")], "the known path's own identity");
    assert!(known.exclusions.is_empty());
    assert_eq!(known.rule.as_deref(), Some("cache.uv"));

    for rel in ["c/node_modules", "d/node_modules"] {
        let excluded = protection(&r, &h, rel);
        assert_eq!(excluded.markers, [h.marker(rel)], "{rel}: the excluded folder's marker is listed");
        assert_eq!(excluded.exclusions, [h.exclusion(rel, Exclude, InSettings, false)], "{rel}");
    }

    // Without a marker the exclusion lists none.
    let plain =
        h.scan(&Settings { excludes: h.paths(&["e/node_modules"]), ..Settings::default() }, &ScanFlags::default());
    assert!(protection(&plain, &h, "e/node_modules").markers.is_empty());
}

#[test]
fn the_apps_protection_fixture_is_a_report_the_cli_could_write() {
    // Written by hand for the app's tests: it reads as a `ScanReport` and
    // writes back to the same JSON, so it holds no field the CLI does not write.
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../app/Packages/DDCKit/Tests/DDCKitTests/Fixtures/scan-report-protection.json");
    let json: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let r: ScanReport = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(r.schema_version, ddc_core::model::SCHEMA_VERSION);
    assert!(r.findings.iter().all(|f| f.rule == "user.kept" && f.protection.is_some()));
    assert_eq!(serde_json::to_value(&r).unwrap(), json);
}
