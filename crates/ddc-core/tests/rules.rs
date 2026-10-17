mod common;

use std::path::PathBuf;

use common::TreeBuilder;
use ddc_core::class::{Removal, RiskClass};
use ddc_core::rules::{Catalog, Classified, Note, RULES, RuleVisitor, classify};
use ddc_core::scan::{self, Progress, SkipReason, WalkOptions, WalkOutput};

const TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n# cache\n";

struct Scan {
    found: Vec<Classified>,
    out: WalkOutput,
}

/// Scans `roots` (relative to the fake home = tree root).
fn scan_with(t: &TreeBuilder, roots: &[&str], excludes: &[&str]) -> Scan {
    let catalog = Catalog::builtin(t.root());
    let excludes = excludes.iter().map(|e| t.path(e).canonicalize().unwrap()).collect();
    let visitor = RuleVisitor::new(&catalog, excludes);
    let roots: Vec<PathBuf> = roots.iter().map(|r| t.path(r)).collect();
    let out =
        scan::walk(&roots, &visitor, &WalkOptions { threads: Some(4), ..Default::default() }, &Progress::default())
            .expect("walk");
    Scan { found: classify(&out, &visitor), out }
}

fn scan(t: &TreeBuilder) -> Scan {
    scan_with(t, &[""], &[])
}

impl Scan {
    fn get(&self, t: &TreeBuilder, rel: &str) -> Option<&Classified> {
        let p = t.path(rel);
        self.found.iter().find(|f| f.owner.path == p)
    }

    fn expect(&self, t: &TreeBuilder, rel: &str, id: &str, class: RiskClass) -> &Classified {
        let f = self.get(t, rel).unwrap_or_else(|| panic!("no finding at {rel}; found: {:?}", self.summary(t)));
        assert_eq!((f.rule.id, f.class), (id, class), "finding at {rel}");
        f
    }

    fn absent(&self, t: &TreeBuilder, rel: &str) {
        assert!(self.get(t, rel).is_none(), "unexpected finding at {rel}: {:?}", self.summary(t));
    }

    fn summary(&self, t: &TreeBuilder) -> Vec<(String, &'static str, RiskClass)> {
        self.found
            .iter()
            .map(|f| (f.owner.path.strip_prefix(t.root()).unwrap().display().to_string(), f.rule.id, f.class))
            .collect()
    }
}

#[test]
fn ambiguous_names_without_markers_are_not_found() {
    let t = TreeBuilder::new();
    t.file("p1/env/notes.txt", 100);
    t.file("p2/target/out.bin", 100);
    t.write("p3/App.xcodeproj/project.pbxproj", "x");
    t.file("p3/build/results.json", 100);
    t.write("p4/src/CMakeLists.txt", "project(x)");
    let src = t.path("p4/src");
    t.write("p4/src/CMakeCache.txt", &format!("CMAKE_HOME_DIRECTORY:INTERNAL={}\n", src.display()));
    t.file("p5/dist/bundle.js", 100);

    let s = scan(&t);

    for rel in ["p1/env", "p2/target", "p3/build", "p4/src", "p5/dist"] {
        s.absent(&t, rel);
    }
    assert!(s.found.is_empty(), "nothing at all, not even a parent: {:?}", s.summary(&t));
}

#[test]
fn project_artifacts_are_found_with_their_class() {
    let t = TreeBuilder::new();
    t.write("py/uv.lock", "");
    t.write("py/.venv/pyvenv.cfg", "home = /usr/bin");
    t.write("py2/requirements-dev.txt", "x");
    t.write("py2/my-env/pyvenv.cfg", "home = /usr/bin");
    t.write("js/package.json", "{}");
    t.write("js/package-lock.json", "{}");
    t.file("js/node_modules/left-pad/index.js", 100);
    t.file("js/node_modules/left-pad/node_modules/inner/index.js", 100);
    t.write("rs/Cargo.toml", "[package]");
    t.file("rs/target/debug/app", 1000);
    t.write("cm/src/CMakeLists.txt", "project(x)");
    let src = t.path("cm/src");
    t.write("cm/out/CMakeCache.txt", &format!("CMAKE_HOME_DIRECTORY:INTERNAL={}\n", src.display()));
    t.write("gr/build.gradle.kts", "");
    t.file("gr/build/classes/A.class", 100);
    t.write("ios/Podfile", "");
    t.write("ios/Podfile.lock", "");
    t.file("ios/Pods/x/y.m", 100);
    t.file("py/__pycache__/m.pyc", 100);
    t.file("py/.pytest_cache/v", 100);
    t.write("tx/tox.ini", "");
    t.file("tx/.tox/py311/f", 100);
    t.write("web/package.json", "{}");
    t.file("web/.next/cache/x", 100);
    t.write("web/.next/package.json", "{\"type\": \"commonjs\"}"); // written by Next.js itself
    t.file("old/node_modules.ddc-deleting-1700000000/x", 100);

    let s = scan(&t);

    s.expect(&t, "py/.venv", "python.venv", RiskClass::Rebuildable);
    s.expect(&t, "py2/my-env", "python.venv", RiskClass::Rebuildable);
    s.expect(&t, "js/node_modules", "node.node_modules", RiskClass::Rebuildable);
    s.absent(&t, "js/node_modules/left-pad/node_modules");
    s.expect(&t, "rs/target", "rust.target", RiskClass::Rebuildable);
    s.expect(&t, "cm/out", "cmake.build", RiskClass::Rebuildable);
    s.expect(&t, "gr/build", "gradle.build", RiskClass::Rebuildable);
    s.expect(&t, "ios/Pods", "cocoapods.pods", RiskClass::Rebuildable);
    s.expect(&t, "py/__pycache__", "python.pycache", RiskClass::Safe);
    s.expect(&t, "py/.pytest_cache", "python.tool-cache", RiskClass::Safe);
    s.expect(&t, "tx/.tox", "python.tox", RiskClass::Safe);
    s.expect(&t, "web/.next", "js.build-cache", RiskClass::Safe);
    s.expect(&t, "old/node_modules.ddc-deleting-1700000000", "ddc.tombstone", RiskClass::Safe);
    let js = s.get(&t, "js/node_modules").unwrap();
    assert_eq!(js.project_root.as_deref(), Some(t.path("js").as_path()));
    assert_eq!(js.has_lockfile, Some(true));
}

#[test]
fn rule_precedence() {
    let t = TreeBuilder::new();
    // uv venv carries a valid CACHEDIR.TAG: the venv rule must win.
    t.write("a/uv.lock", "");
    t.write("a/.venv/pyvenv.cfg", "uv = 0.8");
    t.write("a/.venv/CACHEDIR.TAG", TAG);
    t.write("b/Cargo.toml", "");
    t.write("b/target/CACHEDIR.TAG", TAG);
    t.write("c/tox.ini", "");
    t.write("c/.tox/py311/pyvenv.cfg", "x");
    t.write("d/somecache/CACHEDIR.TAG", TAG);
    t.write("e/notacache/CACHEDIR.TAG", "not the signature");
    // ~/.cargo/registry is tagged but is an ancestor of known paths.
    t.write(".cargo/registry/CACHEDIR.TAG", TAG);
    t.file(".cargo/registry/cache/crate.crate", 1000);
    t.file(".cargo/registry/src/crate/lib.rs", 1000);
    t.file(".cargo/registry/index/config.json", 100);

    let s = scan(&t);

    s.expect(&t, "a/.venv", "python.venv", RiskClass::Rebuildable);
    s.expect(&t, "b/target", "rust.target", RiskClass::Rebuildable);
    s.expect(&t, "c/.tox", "python.tox", RiskClass::Safe);
    s.absent(&t, "c/.tox/py311");
    s.expect(&t, "d/somecache", "generic.cachedir-tag", RiskClass::Review);
    s.absent(&t, "e/notacache");
    s.expect(&t, ".cargo/registry/cache", "cache.cargo", RiskClass::Safe);
    s.expect(&t, ".cargo/registry/src", "cache.cargo", RiskClass::Safe);
    s.absent(&t, ".cargo/registry");
}

#[test]
fn environments_without_lock_file_need_review() {
    let t = TreeBuilder::new();
    t.write("py/.venv/pyvenv.cfg", "x");
    t.write("js/package.json", "{}");
    t.file("js/node_modules/x/i.js", 10);
    t.write("rs/Cargo.toml", "");
    t.file("rs/target/x", 10);

    let s = scan(&t);

    let py = s.expect(&t, "py/.venv", "python.venv", RiskClass::Review);
    assert_eq!((py.has_lockfile, py.notes.as_slice()), (Some(false), &[Note::NoLockfile][..]));
    s.expect(&t, "js/node_modules", "node.node_modules", RiskClass::Review);
    s.expect(&t, "rs/target", "rust.target", RiskClass::Rebuildable);
}

#[test]
fn lock_file_at_the_git_root_counts() {
    let t = TreeBuilder::new();
    t.dir("mono/.git");
    t.write("mono/pnpm-lock.yaml", "");
    t.write("mono/packages/app/package.json", "{}");
    t.file("mono/packages/app/node_modules/x/i.js", 10);
    // worktree: .git is a file
    t.write("wt/.git", "gitdir: /elsewhere/.git/worktrees/wt");
    t.write("wt/uv.lock", "");
    t.write("wt/svc/.venv/pyvenv.cfg", "x");

    let s = scan(&t);

    let f = s.expect(&t, "mono/packages/app/node_modules", "node.node_modules", RiskClass::Rebuildable);
    assert_eq!(f.project_root.as_deref(), Some(t.path("mono").as_path()));
    let w = s.expect(&t, "wt/svc/.venv", "python.venv", RiskClass::Rebuildable);
    assert_eq!(w.project_root.as_deref(), Some(t.path("wt").as_path()));
}

#[test]
fn app_owned_artifacts_need_review() {
    let t = TreeBuilder::new();
    t.write(".local/pipx/venvs/black/pyvenv.cfg", "x");
    t.write(".local/pipx/venvs/requirements.txt", "x");
    t.write(".vscode/extensions/ext/package.json", "{}");
    t.write(".vscode/extensions/ext/package-lock.json", "{}");
    t.file(".vscode/extensions/ext/node_modules/x/i.js", 10);
    t.write(".local/share/nvim/lazy/blink.cmp/Cargo.toml", "");
    t.file(".local/share/nvim/lazy/blink.cmp/target/release/lib.dylib", 100);

    let s = scan(&t);

    for (rel, id) in [
        (".local/pipx/venvs/black", "python.venv"),
        (".vscode/extensions/ext/node_modules", "node.node_modules"),
        (".local/share/nvim/lazy/blink.cmp/target", "rust.target"),
    ] {
        let f = s.expect(&t, rel, id, RiskClass::Review);
        assert!(f.notes.contains(&Note::AppOwned), "{rel}: {:?}", f.notes);
    }
}

#[test]
fn keep_markers_and_excludes_protect_findings() {
    let t = TreeBuilder::new();
    t.write(".cache/uv/.ddc-keep", "");
    t.file(".cache/uv/wheels/a.whl", 100);
    t.write("p1/package.json", "{}");
    t.write("p1/package-lock.json", "{}");
    t.write("p1/node_modules/.ddc-keep", "");
    t.write("p2/package.json", "{}");
    t.write("p2/package-lock.json", "{}");
    t.write("p2/node_modules/patched/.ddc-keep", "");
    t.write("p3/package.json", "{}");
    t.write("p3/package-lock.json", "{}");
    t.file("p3/node_modules/x/i.js", 10);
    t.write("p4/.ddc-keep", "");
    t.write("p4/package.json", "{}");
    t.file("p4/node_modules/x/i.js", 10);
    t.write("Downloads/repo/package.json", "{}");
    t.file("Downloads/repo/node_modules/x/i.js", 10);
    t.file("Downloads/movie.mp4", 1000);

    let s = scan_with(&t, &[""], &["p3/node_modules"]);

    s.expect(&t, ".cache/uv", "user.kept", RiskClass::UserData);
    s.expect(&t, "p1/node_modules", "user.kept", RiskClass::UserData);
    let deep = s.expect(&t, "p2/node_modules", "user.kept", RiskClass::UserData);
    assert_eq!(deep.notes, vec![Note::KeptByMarker]);
    let ex = s.expect(&t, "p3/node_modules", "user.kept", RiskClass::UserData);
    assert_eq!(ex.notes, vec![Note::Excluded]);
    s.expect(&t, "p4", "user.kept", RiskClass::UserData);
    s.absent(&t, "p4/node_modules");
    s.expect(&t, "Downloads", "user.downloads", RiskClass::UserData);
    s.expect(&t, "Downloads/repo/node_modules", "node.node_modules", RiskClass::Review);
}

#[test]
fn known_paths_only_match_inside_scan_roots_and_home() {
    let t = TreeBuilder::new();
    t.file(".cache/uv/wheels/a", 100);
    t.file("Projects/app/.cache/uv/wheels/a", 100);

    let projects = scan_with(&t, &["Projects"], &[]);
    projects.absent(&t, ".cache/uv");
    projects.absent(&t, "Projects/app/.cache/uv");

    let home = scan(&t);
    home.expect(&t, ".cache/uv", "cache.uv", RiskClass::Safe);
    home.absent(&t, "Projects/app/.cache/uv");
}

#[test]
fn global_packages_and_cloud_folders_are_not_findings() {
    let t = TreeBuilder::new();
    t.write(".npm-global/lib/package.json", "{}");
    t.file(".npm-global/lib/node_modules/tool/node_modules/dep/i.js", 10);
    t.file("Library/CloudStorage/GoogleDrive/doc.txt", 10);

    let s = scan(&t);

    assert!(s.found.is_empty(), "{:?}", s.summary(&t));
    assert!(s.out.skipped.iter().any(|k| k.path == t.path("Library/CloudStorage") && k.reason == SkipReason::Excluded));
}

#[test]
fn nvm_default_alias_resolution() {
    let cases: &[(&str, &[&str], &[&str])] = &[
        // (alias/default, extra files, expected findings)
        ("node", &[], &["v18.1.0", "v20.5.0"]),
        ("lts/*", &[], &["v18.1.0", "v22.4.0"]),
        ("18", &[], &["v20.5.0", "v22.4.0"]),
        ("node", &["proj/.nvmrc=20"], &["v18.1.0"]),
        ("system", &[], &[]),
    ];
    for (alias, extra, expected) in cases {
        let t = TreeBuilder::new();
        for v in ["v18.1.0", "v20.5.0", "v22.4.0"] {
            t.file(&format!(".nvm/versions/node/{v}/bin/node"), 100);
            t.file(&format!(".nvm/versions/node/{v}/lib/node_modules/npm/node_modules/x/i.js"), 10);
        }
        t.write(".nvm/alias/default", alias);
        t.write(".nvm/alias/lts/*", "lts/iron");
        t.write(".nvm/alias/lts/iron", "v20.5.0");
        for e in *extra {
            let (rel, content) = e.split_once('=').unwrap();
            t.write(rel, content);
        }

        let s = scan(&t);

        let mut got: Vec<String> = s
            .found
            .iter()
            .filter(|f| f.rule.id == "toolchain.nvm")
            .map(|f| f.owner.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        got.sort();
        assert_eq!(got, *expected, "alias {alias} extra {extra:?}");
        assert!(s.found.iter().all(|f| f.rule.id == "toolchain.nvm"), "{:?}", s.summary(&t));
    }
}

#[test]
fn single_toolchain_version_is_never_a_finding() {
    let t = TreeBuilder::new();
    t.file(".nvm/versions/node/v22.4.0/bin/node", 100);
    t.write(".nvm/alias/default", "node");
    t.file(".pyenv/versions/3.13.1/bin/python", 100);
    t.file(".rustup/toolchains/stable-aarch64-apple-darwin/bin/rustc", 100);

    assert!(scan(&t).found.is_empty());
}

#[test]
fn pyenv_and_rustup_keep_global_and_pinned_versions() {
    let t = TreeBuilder::new();
    for v in ["3.11.9", "3.12.3", "3.13.1"] {
        t.file(&format!(".pyenv/versions/{v}/bin/python"), 100);
    }
    t.write(".pyenv/version", "3.12.3");
    t.write("proj/.python-version", "3.11.9");
    for tc in ["stable-aarch64-apple-darwin", "1.99.1-aarch64-apple-darwin", "nightly-aarch64-apple-darwin"] {
        t.file(&format!(".rustup/toolchains/{tc}/bin/rustc"), 100);
    }
    t.write(".rustup/settings.toml", "default_toolchain = \"stable-aarch64-apple-darwin\"\n");
    t.write("rs/rust-toolchain.toml", "[toolchain]\nchannel = \"1.99.1\"\n");

    let s = scan(&t);

    s.expect(&t, ".pyenv/versions/3.13.1", "toolchain.pyenv", RiskClass::Review);
    s.absent(&t, ".pyenv/versions/3.12.3");
    s.absent(&t, ".pyenv/versions/3.11.9");
    s.expect(&t, ".rustup/toolchains/nightly-aarch64-apple-darwin", "toolchain.rustup", RiskClass::Review);
    s.absent(&t, ".rustup/toolchains/stable-aarch64-apple-darwin");
    s.absent(&t, ".rustup/toolchains/1.99.1-aarch64-apple-darwin");
}

#[test]
fn only_outdated_jetbrains_data_is_found() {
    let t = TreeBuilder::new();
    let base = "Library/Application Support/JetBrains";
    for d in ["PyCharm2023.3", "PyCharm2024.3", "GoLand2024.1", "consentOptions"] {
        t.file(&format!("{base}/{d}/options/x.xml"), 100);
    }

    let s = scan(&t);

    s.expect(&t, &format!("{base}/PyCharm2023.3"), "ide.jetbrains-old-data", RiskClass::Review);
    assert_eq!(s.found.len(), 1, "{:?}", s.summary(&t));
}

#[test]
fn verify_uses_the_same_matching() {
    let t = TreeBuilder::new();
    t.write("py/uv.lock", "");
    t.write("py/.venv/pyvenv.cfg", "x");
    t.file(".cache/uv/a", 10);
    t.write("js/package.json", "{}");
    t.file("js/node_modules/x/i.js", 10);
    let catalog = Catalog::builtin(t.root());
    let venv = catalog.by_id("python.venv").unwrap();
    let uv = catalog.by_id("cache.uv").unwrap();
    let nm = catalog.by_id("node.node_modules").unwrap();

    assert!(catalog.verify(venv, &t.path("py/.venv")));
    assert!(catalog.verify(uv, &t.path(".cache/uv")));
    assert!(catalog.verify(nm, &t.path("js/node_modules")));
    assert!(!catalog.verify(uv, &t.path("py/.venv")));
    // A venv that became a project (`python -m venv .`) no longer verifies.
    t.write("inplace/pyvenv.cfg", "x");
    assert!(catalog.verify(venv, &t.path("inplace")));
    t.write("inplace/pyproject.toml", "");
    assert!(!catalog.verify(venv, &t.path("inplace")));
    std::fs::rename(t.path("js/package.json"), t.path("js/package.json.bak")).unwrap();
    std::fs::rename(t.path("py/.venv/pyvenv.cfg"), t.path("py/.venv/cfg.bak")).unwrap();
    assert!(!catalog.verify(venv, &t.path("py/.venv")));
    assert!(!catalog.verify(nm, &t.path("js/node_modules")));
}

#[test]
fn catalog_invariants() {
    let mut ids: Vec<&str> = RULES.iter().map(|r| r.id).collect();
    ids.sort();
    let before = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), before, "rule ids must be unique");

    for rule in RULES {
        if rule.class == RiskClass::UserData {
            assert_eq!(rule.removal, Removal::None, "{}: user-data is report-only", rule.id);
        }
        if rule.category == ddc_core::class::Category::VmContainer {
            let Removal::NativeOnly(cmds) = rule.removal else { panic!("{}: VM rules must be native-only", rule.id) };
            assert!(!rule.removal.removes_files(), "{}", rule.id);
            for cmd in cmds {
                for banned in ["system", "rm", "volume", "volumes", "container", "--all"] {
                    assert!(!cmd.args.contains(&banned), "{}: forbidden argument {banned} in {:?}", rule.id, cmd.args);
                }
                for arg in cmd.args {
                    let short_cluster = arg.starts_with('-') && !arg.starts_with("--");
                    assert!(!(short_cluster && arg.contains('a')), "{}: `-a` hidden in {arg}", rule.id);
                }
                assert!(cmd.args.contains(&"prune"), "{}: only prune commands: {:?}", rule.id, cmd.args);
            }
        }
        if rule.lockfiles.is_some() {
            assert_eq!(rule.class, RiskClass::Rebuildable, "{}: only rebuildable rules are lock-gated", rule.id);
        }
    }
}

#[test]
fn stray_cmake_caches_and_in_place_venvs_never_claim_projects() {
    let t = TreeBuilder::new();
    // `cd ~/Projects && cmake app` leaves a cache in ~/Projects.
    t.write("Projects/app/CMakeLists.txt", "project(app)");
    let app = t.path("Projects/app");
    t.write("Projects/CMakeCache.txt", &format!("CMAKE_HOME_DIRECTORY:INTERNAL={}\n", app.display()));
    t.file("Projects/thesis/chapter1.tex", 100);
    // `cmake -S src -B .` inside an unpacked tarball.
    t.write("work/tarball/src/CMakeLists.txt", "project(x)");
    let src = t.path("work/tarball/src");
    t.write("work/tarball/CMakeCache.txt", &format!("CMAKE_HOME_DIRECTORY:INTERNAL={}\n", src.display()));
    // A cache in $HOME itself.
    t.write("CMakeCache.txt", &format!("CMAKE_HOME_DIRECTORY:INTERNAL={}\n", app.display()));
    // `python -m venv .` inside a project, and a venv that is a git work tree.
    t.write("proj/pyproject.toml", "");
    t.write("proj/pyvenv.cfg", "x");
    t.dir("gitvenv/.git");
    t.write("gitvenv/pyvenv.cfg", "x");
    // A real out-of-source build still counts.
    t.write("Projects/app/build/CMakeCache.txt", &format!("CMAKE_HOME_DIRECTORY:INTERNAL={}\n", app.display()));
    t.file(".cache/uv/x", 100);

    let s = scan(&t);

    for rel in ["Projects", "work/tarball", "", "proj", "gitvenv"] {
        s.absent(&t, rel);
    }
    s.expect(&t, "Projects/app/build", "cmake.build", RiskClass::Rebuildable);
    s.expect(&t, ".cache/uv", "cache.uv", RiskClass::Safe);
}

#[test]
fn excludes_inside_a_finding_or_above_the_root_protect_it() {
    let t = TreeBuilder::new();
    t.write("p1/package.json", "{}");
    t.write("p1/package-lock.json", "{}");
    t.file("p1/node_modules/patched/index.js", 100);
    t.file(".cache/uv/wheels/a.whl", 100);
    t.file("Projects/foo/__pycache__/m.pyc", 100);

    let s = scan_with(&t, &[""], &["p1/node_modules/patched", ".cache/uv/wheels"]);
    let nm = s.expect(&t, "p1/node_modules", "user.kept", RiskClass::UserData);
    assert_eq!(nm.notes, vec![Note::Excluded]);
    s.expect(&t, ".cache/uv", "user.kept", RiskClass::UserData);

    let above = scan_with(&t, &["Projects/foo"], &["Projects"]);
    above.expect(&t, "Projects/foo", "user.kept", RiskClass::UserData);
    above.absent(&t, "Projects/foo/__pycache__");
}

#[test]
fn pins_inside_protected_projects_still_protect_toolchains() {
    let t = TreeBuilder::new();
    for v in ["v18.1.0", "v20.5.0", "v22.4.0"] {
        t.file(&format!(".nvm/versions/node/{v}/bin/node"), 100);
    }
    t.write(".nvm/alias/default", "22");
    t.write("Projects/legacy/.nvmrc", "# comment line\n18\n");
    t.write("Projects/kept/.ddc-keep", "");
    t.write("Projects/kept/.nvmrc", "20");

    let s = scan_with(&t, &[""], &["Projects/legacy"]);

    let nvm: Vec<_> = s.found.iter().filter(|f| f.rule.id == "toolchain.nvm").collect();
    assert!(nvm.is_empty(), "all versions pinned or default: {:?}", s.summary(&t));
}

#[test]
fn a_tombstone_is_a_tombstone_whatever_it_contains() {
    let t = TreeBuilder::new();
    t.write("old/.venv.ddc-deleting-1700000000-1/pyvenv.cfg", "x");
    t.write("old/build.ddc-deleting-1700000000-2/CMakeCache.txt", "CMAKE_HOME_DIRECTORY:INTERNAL=/nowhere\n");

    let s = scan(&t);

    s.expect(&t, "old/.venv.ddc-deleting-1700000000-1", "ddc.tombstone", RiskClass::Safe);
    s.expect(&t, "old/build.ddc-deleting-1700000000-2", "ddc.tombstone", RiskClass::Safe);
}

#[test]
fn rules_never_fire_inside_app_bundles() {
    let t = TreeBuilder::new();
    t.write("Applications/Tool.app/Contents/Resources/app/package.json", "{}");
    t.write("Applications/Tool.app/Contents/Resources/app/package-lock.json", "{}");
    t.file("Applications/Tool.app/Contents/Resources/app/node_modules/x/i.js", 100);
    t.file(".zcode/Use.app/Contents/Resources/node_modules/y.js", 100);

    let s = scan(&t);

    assert!(s.found.is_empty(), "{:?}", s.summary(&t));
}

#[test]
fn pyenv_prefixes_virtualenvs_and_envs_are_protected() {
    let t = TreeBuilder::new();
    for v in ["3.11.9", "3.12.1", "3.12.3", "3.13.1"] {
        t.file(&format!(".pyenv/versions/{v}/bin/python"), 100);
    }
    t.file(".pyenv/versions/3.11.9/envs/work/bin/python", 100);
    t.symlink(t.path(".pyenv/versions/3.13.1/envs/tools"), ".pyenv/versions/tools");
    t.write("proj/.python-version", "3.12");
    t.write("other/.python-version", "tools");

    let s = scan(&t);

    s.expect(&t, ".pyenv/versions/3.12.1", "toolchain.pyenv", RiskClass::Review);
    for kept in ["3.11.9", "3.12.3", "3.13.1"] {
        s.absent(&t, &format!(".pyenv/versions/{kept}"));
    }
}

#[test]
fn a_keep_marker_inside_downloads_protects_only_its_finding() {
    let t = TreeBuilder::new();
    t.write("Downloads/repo/package.json", "{}");
    t.write("Downloads/repo/node_modules/dep/.ddc-keep", "");
    t.file("Downloads/movie.mkv", 1000);

    let s = scan(&t);

    s.expect(&t, "Downloads", "user.downloads", RiskClass::UserData);
    s.expect(&t, "Downloads/repo/node_modules", "user.kept", RiskClass::UserData);
}

#[test]
fn excluding_a_cloud_folder_still_never_traverses_it() {
    let t = TreeBuilder::new();
    t.file("Library/CloudStorage/Drive/doc.txt", 10);

    let s = scan_with(&t, &[""], &["Library/CloudStorage"]);

    assert!(s.out.skipped.iter().any(|k| k.path == t.path("Library/CloudStorage")));
    s.absent(&t, "Library/CloudStorage");
}

/// Every rule in the catalog has a fixture: a minimal tree that must produce
/// exactly that rule (or nothing, for unlisted rules). A new rule without a
/// fixture fails here.
#[test]
fn every_rule_is_covered_by_a_fixture() {
    use ddc_core::rules::Matcher;
    for rule in RULES {
        let t = TreeBuilder::new();
        let expect: Option<String> = match rule.matcher {
            Matcher::KnownPaths(paths) | Matcher::KnownRegion(paths) => {
                for p in paths {
                    t.file(&format!("{p}/payload"), 100);
                }
                Some(paths[0].to_owned())
            }
            Matcher::NoTraverse(paths) => {
                t.file(&format!("{}/payload", paths[0]), 100);
                None
            }
            Matcher::KnownChildren(parent) => {
                let (old, new, setup): (&str, &str, &[(&str, &str)]) = match rule.id {
                    "toolchain.nvm" => ("v18.0.0", "v20.0.0", &[(".nvm/alias/default", "20")]),
                    "toolchain.pyenv" => ("3.11.0", "3.12.0", &[(".pyenv/version", "3.12.0")]),
                    "toolchain.rustup" => (
                        "nightly-aarch64-apple-darwin",
                        "stable-aarch64-apple-darwin",
                        &[(".rustup/settings.toml", "default_toolchain = \"stable-aarch64-apple-darwin\"\n")],
                    ),
                    "ide.jetbrains-old-data" => ("PyCharm2023.1", "PyCharm2024.1", &[]),
                    other => panic!("no fixture for KnownChildren rule {other}"),
                };
                for v in [old, new] {
                    t.file(&format!("{parent}/{v}/payload"), 100);
                }
                for (rel, content) in setup {
                    t.write(rel, content);
                }
                Some(format!("{parent}/{old}"))
            }
            Matcher::Synthetic => {
                t.write("kept/.ddc-keep", "");
                Some("kept".to_owned())
            }
            _ => {
                let (files, at): (&[(&str, &str)], Option<&str>) = match rule.id {
                    "ddc.tombstone" => (&[("p/x.ddc-deleting-1-1/f", "")], Some("p/x.ddc-deleting-1-1")),
                    "macos.app-bundle" => (&[("A.app/Contents/Resources/node_modules/x.js", "")], None),
                    "node.global-packages" => (&[(".npm-global/lib/node_modules/tool/node_modules/y.js", "")], None),
                    "python.venv" => (&[("p/uv.lock", ""), ("p/.venv/pyvenv.cfg", "")], Some("p/.venv")),
                    "python.tox" => (&[("p/tox.ini", ""), ("p/.tox/py/f", "")], Some("p/.tox")),
                    "python.pycache" => (&[("p/__pycache__/m.pyc", "")], Some("p/__pycache__")),
                    "python.tool-cache" => (&[("p/.mypy_cache/f", "")], Some("p/.mypy_cache")),
                    "node.node_modules" => (
                        &[("p/package.json", ""), ("p/yarn.lock", ""), ("p/node_modules/x.js", "")],
                        Some("p/node_modules"),
                    ),
                    "node.node_modules-orphan" => (&[("p/node_modules/x.js", "")], Some("p/node_modules")),
                    "js.build-cache" => (&[("p/package.json", ""), ("p/.svelte-kit/x", "")], Some("p/.svelte-kit")),
                    "rust.target" => (&[("p/Cargo.toml", ""), ("p/target/debug/x", "")], Some("p/target")),
                    "maven.target" => (&[("p/pom.xml", ""), ("p/target/classes/A.class", "")], Some("p/target")),
                    "gradle.build" => (&[("p/build.gradle", ""), ("p/build/tmp/x", "")], Some("p/build")),
                    "gradle.project-cache" => {
                        (&[("p/settings.gradle.kts", ""), ("p/.gradle/8.0/x", "")], Some("p/.gradle"))
                    }
                    "cmake.build" => (
                        &[("p/CMakeLists.txt", ""), ("p/cmake-build-debug/CMakeCache.txt", "@SRC@")],
                        Some("p/cmake-build-debug"),
                    ),
                    "cocoapods.pods" => {
                        (&[("p/Podfile", ""), ("p/Podfile.lock", ""), ("p/Pods/x", "")], Some("p/Pods"))
                    }
                    "generic.cachedir-tag" => (&[("p/somecache/CACHEDIR.TAG", TAG)], Some("p/somecache")),
                    other => panic!("no fixture for rule {other}: add one here"),
                };
                for (rel, content) in files {
                    let content =
                        content.replace("@SRC@", &format!("CMAKE_HOME_DIRECTORY:INTERNAL={}\n", t.path("p").display()));
                    t.write(rel, &content);
                }
                at.map(str::to_owned)
            }
        };

        let s = scan(&t);

        match &expect {
            Some(rel) => {
                let f = s.get(&t, rel).unwrap_or_else(|| panic!("{}: nothing at {rel}: {:?}", rule.id, s.summary(&t)));
                assert_eq!(f.rule.id, rule.id, "{rel}");
            }
            None => assert!(
                s.found.iter().all(|f| f.rule.id != rule.id) && s.found.is_empty(),
                "{}: unlisted or untraversed rule produced findings: {:?}",
                rule.id,
                s.summary(&t)
            ),
        }
    }
}

/// Known-path rules match only under the home directory, never the same
/// relative path elsewhere.
#[test]
fn known_paths_do_not_match_outside_home() {
    use ddc_core::rules::Matcher;
    let t = TreeBuilder::new();
    for rule in RULES {
        if let Matcher::KnownPaths(paths) = rule.matcher {
            for p in paths {
                t.file(&format!("elsewhere/{p}/payload"), 10);
            }
        }
    }

    let s = scan_with(&t, &["elsewhere"], &[]);

    let known: Vec<_> = s.found.iter().filter(|f| matches!(f.rule.matcher, Matcher::KnownPaths(_))).collect();
    assert!(known.is_empty(), "{:?}", known.iter().map(|f| f.rule.id).collect::<Vec<_>>());
}
