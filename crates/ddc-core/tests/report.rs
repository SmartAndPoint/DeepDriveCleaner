mod common;

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::{OLD, TreeBuilder};
use ddc_core::class::RiskClass;
use ddc_core::env::Env;
use ddc_core::model::{NotSelected, ScanReport};
use ddc_core::plan::{PlanOptions, build_plan};
use ddc_core::platform::statfs::VolumeStats;
use ddc_core::report::volumes::StatFs;
use ddc_core::report::{self, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::scan::{Progress, WalkOptions};
use ddc_core::store;

const DAY: Duration = Duration::from_secs(86_400);

fn set_mtime(path: &Path, when: SystemTime) {
    File::options().write(true).open(path).unwrap().set_modified(when).unwrap();
}

struct Fixture {
    tree: TreeBuilder,
    data: tempfile::TempDir,
    now: SystemTime,
}

impl Fixture {
    fn new() -> Self {
        Self { tree: TreeBuilder::new(), data: tempfile::tempdir().unwrap(), now: SystemTime::now() }
    }

    fn env(&self) -> Env {
        Env::with_paths(self.tree.root(), self.data.path(), &self.data.path().join("logs"), self.now).unwrap()
    }

    fn opts(&self) -> ScanOptions {
        ScanOptions {
            large_file_bytes: 1 << 20,
            walk: WalkOptions { threads: Some(4), ..WalkOptions::default() },
            ..ScanOptions::default()
        }
    }

    fn scan(&self) -> ScanReport {
        report::scan(&self.env(), &self.opts(), &Progress::default()).unwrap()
    }

    fn ago(&self, days: u64) -> SystemTime {
        self.now - DAY * days as u32
    }
}

fn find<'a>(r: &'a ScanReport, t: &TreeBuilder, rel: &str) -> &'a ddc_core::model::Finding {
    let p = t.path(rel);
    r.findings.iter().find(|f| f.path == p).unwrap_or_else(|| panic!("no finding {rel}"))
}

/// Idle project with a venv, an active one with node_modules, a CMake build
/// with no activity signal, a cache, user data and a large file.
fn populated() -> Fixture {
    let fx = Fixture::new();
    let t = &fx.tree;
    t.dir("old/.git");
    t.write("old/.git/index", "x");
    t.write("old/.git/HEAD", "ref: refs/heads/main");
    t.write("old/uv.lock", "");
    t.write("old/.venv/pyvenv.cfg", "home = /usr/bin");
    t.file("old/.venv/lib/site.py", 40_000);
    for f in ["old/.git/index", "old/.git/HEAD", "old/uv.lock"] {
        set_mtime(&t.path(f), fx.ago(30));
    }
    t.write("new/package.json", "{}");
    t.write("new/package-lock.json", "{}");
    t.file("new/node_modules/x/index.js", 20_000);
    set_mtime(&t.path("new/package-lock.json"), fx.ago(2));
    set_mtime(&t.path("new/package.json"), fx.ago(2));
    t.write("cm/src/CMakeLists.txt", "project(x)");
    let src = t.path("cm/src");
    t.write("cm/out/CMakeCache.txt", &format!("CMAKE_HOME_DIRECTORY:INTERNAL={}\n", src.display()));
    t.file(".cache/uv/wheels/a.whl", 60_000);
    t.file("Pictures/photo.jpg", 10_000);
    t.file("Documents/big.iso", 2 << 20);
    fx
}

#[test]
fn findings_carry_class_project_activity_and_selection() {
    if common::unsupported("findings_carry_class_project_activity_and_selection") {
        return;
    }
    let fx = populated();
    let t = &fx.tree;

    let r = fx.scan();

    let venv = find(&r, t, "old/.venv");
    assert_eq!((venv.rule.as_str(), venv.class, venv.selected), ("python.venv", RiskClass::Rebuildable, true));
    let project = venv.project.as_ref().unwrap();
    assert_eq!(
        (project.root.as_path(), project.idle_days, project.has_lockfile),
        (t.path("old").as_path(), Some(30), Some(true))
    );

    let nm = find(&r, t, "new/node_modules");
    assert_eq!(nm.not_selected, Some(NotSelected::ActiveProject { idle_days: 2 }));
    assert!(!nm.selected);

    let cm = find(&r, t, "cm/out");
    assert_eq!((cm.rule.as_str(), cm.not_selected), ("cmake.build", Some(NotSelected::UnknownActivity)));

    let uv = find(&r, t, ".cache/uv");
    assert_eq!((uv.class, uv.selected), (RiskClass::Safe, true));
    assert!(uv.private_bytes.is_some(), "uv cache is clone-prone: private size measured");

    let pics = find(&r, t, "Pictures");
    assert_eq!((pics.class, pics.not_selected), (RiskClass::UserData, Some(NotSelected::UserData)));

    assert_eq!(r.large_files.len(), 1);
    assert_eq!(r.large_files[0].path, t.path("Documents/big.iso"));

    let safe = &r.totals.classes[0];
    assert_eq!((safe.class, safe.count), (RiskClass::Safe, 1));
    assert_eq!(r.totals.selected_bytes, venv.bytes + uv.bytes);
    assert_eq!(r.schema_version, ddc_core::model::SCHEMA_VERSION);
    assert_eq!(r.home, t.root());
}

/// Story "The Trash: its own row, not Your data": the Trash is found, never
/// selected, and its bytes are in no "frees" total.
#[test]
fn the_trash_is_found_but_never_selected_or_counted_as_reclaimable() {
    if common::unsupported("the_trash_is_found_but_never_selected_or_counted_as_reclaimable") {
        return;
    }
    let fx = populated();
    let t = &fx.tree;
    t.file(".Trash/old.zip", 50_000);

    let r = fx.scan();

    let trash = find(&r, t, ".Trash");
    assert_eq!(
        (trash.rule.as_str(), trash.class, trash.selected, trash.not_selected),
        ("trash", RiskClass::Review, false, Some(NotSelected::Review))
    );
    assert!(trash.bytes >= 50_000);
    let (venv, uv) = (find(&r, t, "old/.venv"), find(&r, t, ".cache/uv"));
    assert_eq!(r.totals.selected_reclaimable_bytes, venv.reclaimable_bytes() + uv.reclaimable_bytes());
}

#[test]
fn worktree_activity_comes_from_its_gitdir() {
    if common::unsupported("worktree_activity_comes_from_its_gitdir") {
        return;
    }
    let fx = Fixture::new();
    let t = &fx.tree;
    t.dir("main/.git/worktrees/wt");
    t.write("main/.git/worktrees/wt/index", "x");
    t.write("main/.git/worktrees/wt/HEAD", "ref");
    let gitdir = t.path("main/.git/worktrees/wt");
    t.write("wt/.git", &format!("gitdir: {}\n", gitdir.display()));
    t.write("wt/requirements.txt", "x");
    t.write("wt/.venv/pyvenv.cfg", "x");
    for f in ["main/.git/worktrees/wt/index", "main/.git/worktrees/wt/HEAD", "wt/requirements.txt"] {
        set_mtime(&t.path(f), fx.ago(40));
    }
    set_mtime(&t.path("main/.git/worktrees/wt/index"), fx.ago(5));

    let r = fx.scan();

    let venv = find(&r, t, "wt/.venv");
    assert_eq!(venv.project.as_ref().unwrap().idle_days, Some(5), "index of the worktree's gitdir counts");
    assert_eq!(venv.not_selected, Some(NotSelected::ActiveProject { idle_days: 5 }));
}

#[test]
fn findings_are_ordered_by_class_then_size() {
    if common::unsupported("findings_are_ordered_by_class_then_size") {
        return;
    }
    let r = populated().scan();
    for pair in r.findings.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        assert!(
            a.class < b.class || (a.class == b.class && (a.bytes > b.bytes || (a.bytes == b.bytes && a.path < b.path)))
        );
    }
}

#[test]
fn two_scans_give_the_same_report() {
    if common::unsupported("two_scans_give_the_same_report") {
        return;
    }
    let fx = populated();
    let mut a = fx.scan();
    let mut b = fx.scan();
    for r in [&mut a, &mut b] {
        r.generated_at.clear();
        r.duration_ms = 0;
        // Live free space moves between the two scans.
        r.volumes.iter_mut().for_each(|v| v.free_bytes = 0);
    }
    assert_eq!(a, b);
}

#[test]
fn last_scan_is_written_atomically() {
    if common::unsupported("last_scan_is_written_atomically") {
        return;
    }
    let fx = populated();
    let r = fx.scan();
    let path = store::write_last_scan(&fx.env(), &r).unwrap();
    let names: Vec<String> = std::fs::read_dir(fx.data.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec![store::LAST_SCAN.to_owned()], "no temp files left");
    assert_eq!(path, fx.data.path().join(store::LAST_SCAN));
    assert_eq!(store::read_last_scan(&fx.env()).unwrap(), Some(r));
}

#[test]
fn ignored_findings_keep_their_reason_and_leave_the_totals_and_the_plan() {
    if common::unsupported("ignored_findings_keep_their_reason_and_leave_the_totals_and_the_plan") {
        return;
    }
    let fx = populated();
    let t = &fx.tree;
    t.file("cm/.ddc-deleting-1234/leftover.o", 1000);
    let opts = ScanOptions { disabled_rules: vec!["cache.uv".into()], min_size: 30_000, ..fx.opts() };

    let r = report::scan(&fx.env(), &opts, &Progress::default()).unwrap();

    let uv = find(&r, t, ".cache/uv");
    assert_eq!((uv.ignored, uv.selected, uv.not_selected), (true, false, None), "a switched-off type");
    let nm = find(&r, t, "new/node_modules");
    assert!(nm.ignored && !nm.selected, "under min_size");
    assert_eq!(nm.not_selected, Some(NotSelected::ActiveProject { idle_days: 2 }), "keeps its reason");
    let venv = find(&r, t, "old/.venv");
    assert!(!venv.ignored && venv.selected, "over min_size");
    let tombstone = find(&r, t, "cm/.ddc-deleting-1234");
    assert!(!tombstone.ignored && tombstone.selected, "ddc's own leftover is never hidden");

    let ignored: Vec<_> = r.findings.iter().filter(|f| f.ignored).collect();
    assert_eq!(r.totals.ignored_count, ignored.len() as u64);
    assert_eq!(r.totals.ignored_bytes, ignored.iter().map(|f| f.bytes).sum::<u64>());
    let class_bytes: u64 = r.totals.classes.iter().map(|c| c.bytes).sum();
    assert_eq!(class_bytes + r.totals.ignored_bytes, r.findings.iter().map(|f| f.bytes).sum::<u64>());
    assert_eq!(r.totals.selected_bytes, venv.bytes + tombstone.bytes);

    let plan = build_plan(&r, &Catalog::builtin(t.root()), &PlanOptions::default());
    assert!(plan.items.iter().all(|i| !i.finding.ignored), "a plan never holds an ignored finding");
    assert!(plan.items.iter().any(|i| i.finding.path == tombstone.path));
}

#[test]
fn a_report_without_the_new_fields_still_decodes_and_they_stay_optional() {
    if common::unsupported("a_report_without_the_new_fields_still_decodes_and_they_stay_optional") {
        return;
    }
    let mut json = serde_json::to_value(populated().scan()).unwrap();
    let obj = json.as_object_mut().unwrap();
    obj.remove("volumes");
    let findings = obj["findings"].as_array_mut().unwrap();
    assert!(findings.iter().any(|f| f["modified"].is_string()), "{findings:#?}");
    for f in findings {
        f.as_object_mut().unwrap().remove("ignored");
        f.as_object_mut().unwrap().remove("modified");
    }
    serde_json::from_value::<ScanReport>(json).expect("a 0.1.0 report decodes");

    let schema = serde_json::to_value(schemars::schema_for!(ScanReport)).unwrap();
    let required = |v: &serde_json::Value| -> Vec<String> {
        v["required"].as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_owned()).collect()
    };
    assert!(!required(&schema).contains(&"volumes".to_owned()));
    assert!(!required(&schema["$defs"]["Finding"]).contains(&"ignored".to_owned()));
    // Story "Review window: sort a group's rows by name, size or last change":
    // an optional date, under the same version.
    assert!(!required(&schema["$defs"]["Finding"]).contains(&"modified".to_owned()));
    assert_eq!(schema["$defs"]["Finding"]["properties"]["modified"]["type"], serde_json::json!(["string", "null"]));
    let totals = required(&schema["$defs"]["Totals"]);
    assert!(!totals.contains(&"ignored_count".to_owned()) && !totals.contains(&"ignored_bytes".to_owned()));
    assert_eq!(ddc_core::model::SCHEMA_VERSION, 2);
}

/// Story "Review window: sort a group's rows by name, size or last change":
/// a finding's `modified` is the newest change inside it, fixed-width UTC,
/// wherever that change lies; a region, a user-data folder and a root that
/// cannot be opened have none. Without the times no finding has one, and a
/// user-data folder's entries keep their own.
#[test]
fn a_finding_reports_the_newest_change_inside_it() {
    if common::unsupported("a_finding_reports_the_newest_change_inside_it") {
        return;
    }
    let fx = Fixture::new();
    let t = &fx.tree;
    t.write("proj/.venv/pyvenv.cfg", "home = /usr/bin\n");
    t.file("proj/.venv/bin/python", 1_000);
    t.file("proj/.venv/lib/python3.12/site-packages/old.py", 1_000);
    t.file("proj/.venv/lib/python3.12/site-packages/new.py", 1_000);
    t.write("web/package.json", "{}");
    t.write("web/node_modules/.package-lock.json", "{}");
    t.file("web/node_modules/left-pad/index.js", 1_000);
    t.write("linked/package.json", "{}");
    let stored = t.file("store/index.js", 1_000);
    t.hardlink(&stored, "linked/node_modules/dep/index.js");
    t.file("linked/node_modules/dep/README", 1_000);
    t.file("Downloads/a.zip", 1_000);
    t.file("Pictures/one.jpg", 1_000);
    t.file("Library/Caches/Homebrew/x.tar.gz", 1_000);
    // Kept by a marker at its top, by one deep inside, by an exclusion inside;
    // and one left alone.
    for p in ["a", "b", "c", "g"] {
        t.write(&format!("{p}/package.json"), "{}");
        t.file(&format!("{p}/node_modules/dep/index.js"), 1_000);
    }
    t.write("a/node_modules/.ddc-keep", "");
    t.write("b/node_modules/dep/.ddc-keep", "");
    t.age("", OLD);
    for p in ["a", "b", "c", "g"] {
        t.touch(&format!("{p}/node_modules/dep/index.js"), OLD + 50);
    }
    // Three levels down; a direct child of a root claimed after its listing; a hard link.
    t.touch("proj/.venv/lib/python3.12/site-packages/new.py", OLD + 10);
    t.touch("web/node_modules/.package-lock.json", OLD + 20);
    t.touch("store/index.js", OLD + 30);
    t.touch("Downloads/a.zip", OLD + 40);
    t.touch("Pictures/one.jpg", OLD + 40);
    let brew = t.lock("Library/Caches/Homebrew");
    let enforced = std::fs::read_dir(&brew).is_err();

    for claim_times in [true, false] {
        let mut opts = fx.opts();
        opts.walk.claim_times = claim_times;
        opts.contents.min_bytes = 1;
        opts.excludes = vec![t.path("c/node_modules/dep")];
        let r = report::scan(&fx.env(), &opts, &Progress::default()).unwrap();
        let modified = |rel: &str| find(&r, t, rel).modified.clone();
        let on = |at: &str| claim_times.then(|| at.to_owned());
        assert_eq!(modified("proj/.venv"), on("2024-01-01T00:00:10Z"), "times {claim_times}");
        assert_eq!(modified("web/node_modules"), on("2024-01-01T00:00:20Z"), "times {claim_times}");
        assert_eq!(modified("linked/node_modules"), on("2024-01-01T00:00:30Z"), "times {claim_times}");
        for none in ["Downloads", "Pictures"] {
            assert_eq!(modified(none), None, "{none}, times {claim_times}");
        }
        // A protected finding has no date, also one kept only after its walk.
        for kept in ["a/node_modules", "b/node_modules", "c/node_modules"] {
            let f = find(&r, t, kept);
            assert_eq!((f.rule.as_str(), &f.modified), ("user.kept", &None), "{kept}, times {claim_times}");
        }
        assert_eq!(modified("g/node_modules"), on("2024-01-01T00:00:50Z"), "times {claim_times}");
        if enforced {
            assert_eq!(modified("Library/Caches/Homebrew"), None, "times {claim_times}");
        }
        for folder in ["Downloads", "Pictures"] {
            let entries = &find(&r, t, folder).contents.as_ref().unwrap().entries;
            let times: Vec<_> = entries.iter().map(|e| e.modified.as_deref()).collect();
            assert_eq!(times, [Some("2024-01-01T00:00:40Z")], "{folder}, times {claim_times}");
        }
    }
}

/// Roots under `v2/` live on a second volume; `gone/` cannot be read.
struct TwoVolumes(PathBuf);

impl StatFs for TwoVolumes {
    fn statfs(&self, path: &Path) -> io::Result<VolumeStats> {
        let (mount, free) = if path.starts_with(self.0.join("gone")) {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        } else if path.starts_with(self.0.join("v2")) {
            ("/Volumes/Two", 5)
        } else {
            ("/System/Volumes/Data", 50)
        };
        Ok(VolumeStats {
            fs_type: "apfs".into(),
            mount_point: mount.into(),
            available_bytes: free,
            total_bytes: 100,
            free_bytes: free,
            mounted_from: "/dev/disk9s1".into(),
        })
    }
}

#[test]
fn volumes_list_each_volume_once_with_its_roots() {
    if common::unsupported("volumes_list_each_volume_once_with_its_roots") {
        return;
    }
    let fx = Fixture::new();
    let t = &fx.tree;
    for d in ["a", "b", "v2/c", "gone"] {
        t.dir(d);
    }
    let opts = ScanOptions { roots: ["a", "b", "v2/c", "gone"].map(|d| t.path(d)).to_vec(), ..fx.opts() };

    let r = report::scan_with(&fx.env(), &opts, &Progress::default(), &TwoVolumes(t.root().to_path_buf())).unwrap();

    let got: Vec<(&Path, Vec<PathBuf>, u64)> =
        r.volumes.iter().map(|v| (v.mount_point.as_path(), v.roots.clone(), v.free_bytes)).collect();
    assert_eq!(
        got,
        [
            (Path::new("/System/Volumes/Data"), vec![t.path("a"), t.path("b")], 50),
            (Path::new("/Volumes/Two"), vec![t.path("v2/c")], 5),
        ],
        "one entry per volume; the unreadable one is left out"
    );
}

#[test]
fn exclusive_lock_blocks_a_scan() {
    let fx = Fixture::new();
    let env = fx.env();
    let clean = store::try_lock(&env, store::LockMode::Exclusive).unwrap();
    assert!(matches!(store::try_lock(&env, store::LockMode::Shared), Err(store::LockError::Busy(_))));
    drop(clean);
    let _a = store::try_lock(&env, store::LockMode::Shared).unwrap();
    let _b = store::try_lock(&env, store::LockMode::Shared).expect("scans share the lock");
}

#[test]
#[allow(clippy::disallowed_methods)] // DDC_UPDATE_SCHEMA regenerates the committed file
fn committed_json_schema_is_current() {
    let schema = serde_json::to_string_pretty(&schemars::schema_for!(ScanReport)).unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/scan.schema.json");
    if std::env::var_os("DDC_UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &schema).unwrap();
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(committed == schema, "docs/scan.schema.json is stale: run DDC_UPDATE_SCHEMA=1 cargo test");
}

#[test]
fn fresh_manifest_under_a_stale_git_root_keeps_the_project_active() {
    if common::unsupported("fresh_manifest_under_a_stale_git_root_keeps_the_project_active") {
        return;
    }
    let fx = Fixture::new();
    let t = &fx.tree;
    t.dir("mono/.git");
    t.write("mono/.git/index", "x");
    t.write("mono/.git/HEAD", "ref");
    t.write("mono/package.json", "{}");
    t.write("mono/pnpm-lock.yaml", "");
    for f in ["mono/.git/index", "mono/.git/HEAD", "mono/package.json", "mono/pnpm-lock.yaml"] {
        set_mtime(&t.path(f), fx.ago(120));
    }
    t.write("mono/packages/a/package.json", "{}");
    t.file("mono/packages/a/node_modules/x/i.js", 1000);
    set_mtime(&t.path("mono/packages/a/package.json"), fx.ago(1));

    let r = fx.scan();

    let nm = find(&r, t, "mono/packages/a/node_modules");
    assert_eq!(nm.project.as_ref().unwrap().root, t.path("mono"));
    assert_eq!(nm.not_selected, Some(NotSelected::ActiveProject { idle_days: 1 }));
}

#[test]
fn a_git_repository_in_home_is_not_a_project_root() {
    if common::unsupported("a_git_repository_in_home_is_not_a_project_root") {
        return;
    }
    let fx = Fixture::new();
    let t = &fx.tree;
    t.dir(".git");
    t.write(".git/index", "x");
    t.write("proj/package.json", "{}");
    t.write("proj/package-lock.json", "{}");
    t.file("proj/node_modules/x/i.js", 1000);

    let r = fx.scan();

    assert_eq!(find(&r, t, "proj/node_modules").project.as_ref().unwrap().root, t.path("proj"));
}

#[test]
fn project_root_never_goes_above_the_scan_root() {
    if common::unsupported("project_root_never_goes_above_the_scan_root") {
        return;
    }
    let fx = Fixture::new();
    let t = &fx.tree;
    t.dir("repo/.git");
    t.write("repo/app/package.json", "{}");
    t.file("repo/app/node_modules/x/i.js", 1000);
    let opts = ScanOptions {
        roots: vec![t.path("repo/app/node_modules")],
        walk: WalkOptions { threads: Some(2), ..WalkOptions::default() },
        ..ScanOptions::default()
    };

    let r = report::scan(&fx.env(), &opts, &Progress::default()).unwrap();

    for f in &r.findings {
        if let Some(p) = &f.project {
            assert!(
                !t.path("repo").ends_with(&p.root) || p.root != t.path("repo"),
                "{:?} escaped the scan root",
                p.root
            );
        }
    }
}

#[test]
fn large_files_inside_findings_are_not_listed_but_region_remainders_are() {
    if common::unsupported("large_files_inside_findings_are_not_listed_but_region_remainders_are") {
        return;
    }
    let fx = Fixture::new();
    let t = &fx.tree;
    t.write("p/package.json", "{}");
    t.file("p/node_modules/big.bin", 2 << 20);
    t.file("Downloads/movie.mkv", 2 << 20);
    t.file("Movies/clip.mov", 2 << 20);

    let r = fx.scan();

    let listed: Vec<&Path> = r.large_files.iter().map(|f| f.path.as_path()).collect();
    assert_eq!(listed, vec![t.path("Downloads/movie.mkv").as_path()]);
}

/// "Skip entirely" (safety ADR, amendment of 2026-10-28): a skip is never
/// opened and is listed in `not_scanned`, canonical; one inside a claimed
/// finding is read with it, keeps it whole and is not listed.
#[test]
fn a_skipped_folder_is_never_opened_and_is_listed_as_not_scanned() {
    if common::unsupported("a_skipped_folder_is_never_opened_and_is_listed_as_not_scanned") {
        return;
    }
    use ddc_core::settings::{ScanFlags, Settings};
    let fx = Fixture::new();
    let t = &fx.tree;
    t.write("Work/Raw/proj/package.json", "{}");
    t.write("Work/Raw/proj/package-lock.json", "{}");
    t.file("Work/Raw/proj/node_modules/x/index.js", 20_000);
    t.file("Work/Raw/locked/x", 100);
    let locked = t.lock("Work/Raw/locked");
    t.file(".npm/_cacache/index-v5/a", 10_000);
    t.symlink(t.path("Work/Raw"), "link-to-raw");
    let settings = Settings { skips: vec![t.path(".npm/_cacache/index-v5")], ..Settings::default() };
    let resolved =
        settings.resolve(&fx.env(), &ScanFlags { skips: vec![t.path("link-to-raw")], ..ScanFlags::default() }).unwrap();
    let opts = ScanOptions { ..resolved.options };

    let r = report::scan(&fx.env(), &ScanOptions { large_file_bytes: 1 << 20, ..opts }, &Progress::default()).unwrap();

    assert_eq!(r.not_scanned, [t.path("Work/Raw").canonicalize().unwrap()], "canonical, the claimed one not listed");
    assert!(r.inaccessible.iter().all(|i| i.path != locked), "never opened: {:?}", r.inaccessible);
    assert!(!r.findings.iter().any(|f| f.path.starts_with(t.path("Work/Raw"))), "nothing inside is a finding");
    let npm = find(&r, t, ".npm/_cacache");
    assert_eq!(npm.rule, ddc_core::rules::KEPT, "a skip inside a claim keeps the whole finding");
    assert!(!npm.selected);

    let mut json = serde_json::to_value(&r).unwrap();
    json.as_object_mut().unwrap().remove("not_scanned");
    assert!(serde_json::from_value::<ScanReport>(json).unwrap().not_scanned.is_empty(), "optional");
    assert_eq!(ddc_core::model::SCHEMA_VERSION, 2);
}

#[test]
fn unreadable_folders_are_reported() {
    if common::unsupported("unreadable_folders_are_reported") {
        return;
    }
    let fx = Fixture::new();
    let t = &fx.tree;
    t.file("secret/x", 100);
    let locked = t.lock("secret");
    if std::fs::read_dir(&locked).is_ok() {
        eprintln!("SKIPPED: running as root");
        return;
    }

    let r = fx.scan();

    assert!(!r.full_disk_access, "a temp HOME has no TCC.db");
    assert_eq!(r.inaccessible.len(), 1);
    assert_eq!(r.inaccessible[0].path, locked);
}

#[test]
fn app_bundles_count_as_outside_findings_and_their_large_files_are_listed() {
    if common::unsupported("app_bundles_count_as_outside_findings_and_their_large_files_are_listed") {
        return;
    }
    let fx = Fixture::new();
    let t = &fx.tree;
    t.file("Applications/Big.app/Contents/Resources/model.bin", 2 << 20);
    t.write("p/.venv/pyvenv.cfg", "x");

    let r = fx.scan();

    assert!(r.findings.iter().all(|f| !f.path.starts_with(t.path("Applications"))));
    assert_eq!(r.large_files.len(), 1);
    assert!(r.totals.unowned_bytes >= 2 << 20, "bundle bytes are reported as outside findings");
    let venv = find(&r, t, "p/.venv");
    assert_eq!(
        (venv.class, venv.removal),
        (RiskClass::Review, ddc_core::model::RemovalKind::Trash),
        "no lock file: review → Trash"
    );
}
