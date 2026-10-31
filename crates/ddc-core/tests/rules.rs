mod common;

use std::path::PathBuf;

use common::fixtures::{TAG, rule_fixture};
use common::{OLD, TreeBuilder};
use ddc_core::class::{Removal, RiskClass};
use ddc_core::rules::{Catalog, Classified, Note, RULES, RuleVisitor, classify};
use ddc_core::scan::{self, Progress, ReaderKind, SkipReason, WalkOptions, WalkOutput};

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
    if common::unsupported("ambiguous_names_without_markers_are_not_found") {
        return;
    }
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
    if common::unsupported("project_artifacts_are_found_with_their_class") {
        return;
    }
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
    if common::unsupported("rule_precedence") {
        return;
    }
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
    if common::unsupported("environments_without_lock_file_need_review") {
        return;
    }
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
    if common::unsupported("lock_file_at_the_git_root_counts") {
        return;
    }
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
    if common::unsupported("app_owned_artifacts_need_review") {
        return;
    }
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
    if common::unsupported("keep_markers_and_excludes_protect_findings") {
        return;
    }
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
    if common::unsupported("known_paths_only_match_inside_scan_roots_and_home") {
        return;
    }
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

/// The Hugging Face rule finds the downloads, never the sign-in token that
/// sits at the cache's root beside them.
#[test]
fn hugging_face_downloads_are_found_and_its_token_is_kept() {
    if common::unsupported("hugging_face_downloads_are_found_and_its_token_is_kept") {
        return;
    }
    let t = TreeBuilder::new();
    t.file(".cache/huggingface/hub/models--org--name/blobs/a", 100);
    t.file(".cache/huggingface/xet/chunks/b", 100);
    t.file(".cache/huggingface/datasets/set/c", 100);
    t.write(".cache/huggingface/token", "hf_x");
    t.write(".cache/huggingface/stored_tokens", "[x]");

    let s = scan(&t);

    for dir in ["hub", "xet", "datasets"] {
        s.expect(&t, &format!(".cache/huggingface/{dir}"), "model.huggingface", RiskClass::Review);
    }
    s.absent(&t, ".cache/huggingface");
    assert_eq!(s.found.len(), 3, "{:?}", s.summary(&t));
}

#[test]
fn global_packages_and_cloud_folders_are_not_findings() {
    if common::unsupported("global_packages_and_cloud_folders_are_not_findings") {
        return;
    }
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
    if common::unsupported("nvm_default_alias_resolution") {
        return;
    }
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
    if common::unsupported("single_toolchain_version_is_never_a_finding") {
        return;
    }
    let t = TreeBuilder::new();
    t.file(".nvm/versions/node/v22.4.0/bin/node", 100);
    t.write(".nvm/alias/default", "node");
    t.file(".pyenv/versions/3.13.1/bin/python", 100);
    t.file(".rustup/toolchains/stable-aarch64-apple-darwin/bin/rustc", 100);

    assert!(scan(&t).found.is_empty());
}

#[test]
fn pyenv_and_rustup_keep_global_and_pinned_versions() {
    if common::unsupported("pyenv_and_rustup_keep_global_and_pinned_versions") {
        return;
    }
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
    if common::unsupported("only_outdated_jetbrains_data_is_found") {
        return;
    }
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
    // `cache.uv` is `~/.cache/uv` on macOS; Windows has its own path.
    let catalog = Catalog::from_rules(ddc_core::rules::rules_of(ddc_core::platform::Platform::Macos), t.root());
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

/// ADR "Agent footprint: worktrees, harness data and bounded consent for
/// agents", §3: a harness's history, sessions and live records are never
/// in a `safe` rule, and every `review` rule of a harness is `listed_only`.
#[test]
fn no_safe_agent_rule_names_a_harness_history() {
    use ddc_core::platform::Platform;
    use ddc_core::rules::{Matcher, rules_of};
    let history = ["paste-cache", "usage-data", "file-history", "projects", "shell-snapshots", "sessions"];
    let agents: Vec<_> = rules_of(Platform::Macos).iter().filter(|r| r.id.starts_with("agent.")).collect();
    assert!(!agents.is_empty());
    for rule in agents {
        let paths: Vec<&str> = match rule.matcher {
            Matcher::KnownPaths(p) => p.to_vec(),
            Matcher::KnownChildren(parent) => vec![parent],
            _ => panic!("{}: an agent rule names its paths", rule.id),
        };
        if rule.class == RiskClass::Safe {
            for p in paths {
                let last = p.rsplit('/').next().unwrap();
                assert!(!history.contains(&last), "{}: {p} is in a safe rule", rule.id);
            }
        }
        assert_eq!(rule.listed_only, rule.class == RiskClass::Review, "{}", rule.id);
        assert!(rule.owners.is_empty(), "{}: no owner by process name", rule.id);
    }
}

#[test]
fn stray_cmake_caches_and_in_place_venvs_never_claim_projects() {
    if common::unsupported("stray_cmake_caches_and_in_place_venvs_never_claim_projects") {
        return;
    }
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
    if common::unsupported("excludes_inside_a_finding_or_above_the_root_protect_it") {
        return;
    }
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
    if common::unsupported("pins_inside_protected_projects_still_protect_toolchains") {
        return;
    }
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
    if common::unsupported("a_tombstone_is_a_tombstone_whatever_it_contains") {
        return;
    }
    let t = TreeBuilder::new();
    t.write("old/.venv.ddc-deleting-1700000000-1/pyvenv.cfg", "x");
    t.write("old/build.ddc-deleting-1700000000-2/CMakeCache.txt", "CMAKE_HOME_DIRECTORY:INTERNAL=/nowhere\n");

    let s = scan(&t);

    s.expect(&t, "old/.venv.ddc-deleting-1700000000-1", "ddc.tombstone", RiskClass::Safe);
    s.expect(&t, "old/build.ddc-deleting-1700000000-2", "ddc.tombstone", RiskClass::Safe);
}

#[test]
fn rules_never_fire_inside_app_bundles() {
    if common::unsupported("rules_never_fire_inside_app_bundles") {
        return;
    }
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
    if common::unsupported("pyenv_prefixes_virtualenvs_and_envs_are_protected") {
        return;
    }
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
    if common::unsupported("a_keep_marker_inside_downloads_protects_only_its_finding") {
        return;
    }
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
    if common::unsupported("excluding_a_cloud_folder_still_never_traverses_it") {
        return;
    }
    let t = TreeBuilder::new();
    t.file("Library/CloudStorage/Drive/doc.txt", 10);

    let s = scan_with(&t, &[""], &["Library/CloudStorage"]);

    assert!(s.out.skipped.iter().any(|k| k.path == t.path("Library/CloudStorage")));
    s.absent(&t, "Library/CloudStorage");
}

/// Every rule sits in exactly one group of the Types settings. A new rule
/// without a group fails here. The macOS catalog holds every group's rules,
/// on every OS the tests run on.
#[test]
fn every_rule_has_a_group() {
    use ddc_core::platform::Platform;
    use ddc_core::rules::listing::{GROUPS, RuleList, group_of};
    use ddc_core::rules::rules_of;
    let macos = rules_of(Platform::Macos);
    for rule in macos {
        let groups = GROUPS.iter().filter(|(_, ids)| ids.contains(&rule.id)).count();
        assert_eq!(groups, 1, "{} is in {groups} groups", rule.id);
    }
    for (label, ids) in GROUPS {
        assert!(!ids.is_empty(), "{label} is empty");
        for id in *ids {
            assert!(macos.iter().any(|r| r.id == *id), "{label} names an unknown rule {id}");
        }
    }
    let list = RuleList::for_platform(Platform::Macos);
    assert_eq!(list.groups.len(), GROUPS.len(), "every group has a macOS rule");
    assert_eq!(
        list.rules.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        macos.iter().map(|r| r.id).collect::<Vec<_>>()
    );
    // The coding agents' sealed folders, and their report-only rules until
    // a liveness check lets them go to the Trash (story "Harness data rules").
    const AGENTS_FIXED: &[&str] = &[
        "agent.claude-code-sealed",
        "agent.cursor-sealed",
        "agent.codex-sessions",
        "agent.codex-runtimes",
        "agent.opencode-data",
        "agent.cursor-agent-data",
    ];
    let agents_fixed = |r: &&ddc_core::rules::listing::RuleInfo| AGENTS_FIXED.contains(&r.id.as_str());
    let fixed: Vec<&str> =
        list.rules.iter().filter(|r| !r.switchable && !agents_fixed(r)).map(|r| r.group.as_str()).collect();
    assert!(
        fixed.iter().all(|g| ["Your data", "Trash", "Interrupted cleanups", "VM and container images"].contains(g)),
        "only user data, the Trash, ddc's leftovers and tool-only runtimes have no switch: {fixed:?}"
    );
    assert!(!list.rules.iter().find(|r| r.id == "ddc.tombstone").unwrap().switchable);
    assert!(!list.rules.iter().find(|r| r.id == "vm.colima").unwrap().switchable, "a tool-only rule with no command");
    assert!(list.rules.iter().find(|r| r.id == "cache.npm").unwrap().switchable);
    assert_eq!(group_of("cache.colima"), Some("VM and container images"));
    assert_eq!(group_of("trash"), Some("Trash"));
}

#[test]
#[allow(clippy::disallowed_methods)] // DDC_UPDATE_SCHEMA regenerates the committed file
fn committed_rules_schema_is_current() {
    let schema =
        serde_json::to_string_pretty(&schemars::schema_for!(ddc_core::rules::listing::RuleList)).unwrap() + "\n";
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/rules.schema.json");
    if std::env::var_os("DDC_UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &schema).unwrap();
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(committed == schema, "docs/rules.schema.json is stale: run DDC_UPDATE_SCHEMA=1 cargo test");
}

/// Every rule in the catalog has a fixture: a minimal tree that must produce
/// exactly that rule (or nothing, for unlisted rules). A new rule without a
/// fixture fails here.
#[test]
fn every_rule_is_covered_by_a_fixture() {
    if common::unsupported("every_rule_is_covered_by_a_fixture") {
        return;
    }
    for rule in RULES {
        let t = TreeBuilder::new();
        let expect = rule_fixture(&t, rule);

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
    if common::unsupported("known_paths_do_not_match_outside_home") {
        return;
    }
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

/// Story "Review window: sort a group's rows by name, size or last change": a
/// finding holds the newest change inside it, wherever it lies; a user-data
/// folder, a region, a protected or unlisted claim and a root that cannot be
/// opened hold none. With the times off, no finding has one, and a user-data
/// folder's entries keep theirs.
#[test]
fn a_finding_holds_the_newest_change_inside_it() {
    if common::unsupported("a_finding_holds_the_newest_change_inside_it") {
        return;
    }
    let t = TreeBuilder::new();
    t.write("proj/.venv/pyvenv.cfg", "home = /usr/bin\n");
    t.file("proj/.venv/bin/python", 100);
    t.file("proj/.venv/lib/python3.12/site-packages/old.py", 100);
    t.file("proj/.venv/lib/python3.12/site-packages/new.py", 100);
    t.write("web/package.json", "{}");
    t.write("web/node_modules/.package-lock.json", "{}");
    t.file("web/node_modules/left-pad/index.js", 100);
    t.write("linked/package.json", "{}");
    let stored = t.file("store/index.js", 100);
    t.hardlink(&stored, "linked/node_modules/dep/index.js");
    t.file("linked/node_modules/dep/README", 100);
    t.write("kept/.ddc-keep", "");
    t.file("kept/a", 100);
    t.file("Tool.app/Contents/MacOS/tool", 100);
    t.file("Downloads/a.zip", 100);
    t.file("Downloads/sub/b", 100);
    t.file("Pictures/one.jpg", 100);
    t.file("Library/Caches/Homebrew/x.tar.gz", 100);
    t.age("", OLD);
    // Three levels down; a direct child of a root claimed after its listing; a hard link.
    t.touch("proj/.venv/lib/python3.12/site-packages/new.py", OLD + 10);
    t.touch("web/node_modules/.package-lock.json", OLD + 20);
    t.touch("store/index.js", OLD + 30);
    for rel in ["kept/a", "Tool.app/Contents/MacOS/tool", "Downloads/a.zip", "Downloads/sub", "Pictures/one.jpg"] {
        t.touch(rel, OLD + 40);
    }
    let brew = t.lock("Library/Caches/Homebrew");
    let enforced = std::fs::read_dir(&brew).is_err();
    let catalog = Catalog::builtin(t.root());
    let visitor = RuleVisitor::new(&catalog, Vec::new());

    for reader in [ReaderKind::Bulk, ReaderKind::Std] {
        for claim_times in [true, false] {
            let opts = WalkOptions { threads: Some(4), reader, claim_times, ..WalkOptions::default() };
            let out = scan::walk(&[t.root().to_path_buf()], &visitor, &opts, &Progress::default()).expect("walk");
            let modified = |rel: &str| {
                let o = out.owners.iter().find(|o| o.path == t.path(rel));
                o.unwrap_or_else(|| panic!("no owner {rel} in {:#?}", out.owners)).modified
            };
            let on = |secs| claim_times.then_some(secs);
            let case = format!("{reader:?}, times {claim_times}");
            assert_eq!(modified("proj/.venv"), on(OLD + 10), "{case}");
            assert_eq!(modified("web/node_modules"), on(OLD + 20), "{case}");
            assert_eq!(modified("linked/node_modules"), on(OLD + 30), "{case}");
            for none in ["kept", "Tool.app", "Downloads", "Pictures"] {
                assert_eq!(modified(none), None, "{none}, {case}");
            }
            if enforced {
                assert_eq!(modified("Library/Caches/Homebrew"), None, "{case}");
            }
            let downloads = out.entries.iter().find(|e| e.owner == t.path("Downloads")).expect("Downloads listed");
            let times: Vec<_> = downloads.entries.iter().map(|e| e.modified).collect();
            assert_eq!(times, [Some(OLD + 40); 2], "{case}");
        }
    }
}
