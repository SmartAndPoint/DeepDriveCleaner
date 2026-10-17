mod common;

use std::fs::File;
use std::path::Path;
use std::time::{Duration, SystemTime};

use common::TreeBuilder;
use ddc_core::class::RiskClass;
use ddc_core::env::Env;
use ddc_core::model::{NotSelected, ScanReport};
use ddc_core::report::{self, ScanOptions};
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

    fn scan(&self) -> ScanReport {
        let opts = ScanOptions {
            large_file_bytes: 1 << 20,
            walk: WalkOptions { threads: Some(4), ..WalkOptions::default() },
            ..ScanOptions::default()
        };
        report::scan(&self.env(), &opts, &Progress::default()).unwrap()
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

#[test]
fn worktree_activity_comes_from_its_gitdir() {
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
    let fx = populated();
    let mut a = fx.scan();
    let mut b = fx.scan();
    for r in [&mut a, &mut b] {
        r.generated_at.clear();
        r.duration_ms = 0;
    }
    assert_eq!(a, b);
}

#[test]
fn last_scan_is_written_atomically() {
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

#[test]
fn unreadable_folders_are_reported() {
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
