//! The background path cannot reach the cleanup executor (ADR "Cleanup safety
//! model", §8): its sources do not mention it.

use std::path::Path;

#[test]
fn background_sources_never_reference_cleanup() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for file in [
        "cmd/background.rs",
        "cmd/notify.rs",
        "notify.rs",
        "platform/launchd.rs",
        "platform/macos.rs",
        "platform/unported.rs",
    ] {
        let text = std::fs::read_to_string(src.join(file)).unwrap();
        let code: String = text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        for forbidden in ["exec::", "plan::", "Executor", "build_plan", "trash", "remove_dir_all", "Consent"] {
            assert!(!code.contains(forbidden), "{file} mentions {forbidden}");
        }
    }
}

/// The background scan reaches osascript only through the `Notifier` the
/// dispatcher hands it, so a test double sees every notification.
#[test]
fn the_background_scan_runs_no_program_itself() {
    let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cmd/background.rs")).unwrap();
    let code: String = text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    for forbidden in ["Osascript", "osascript", "Command::new", "process::Command", "/usr/bin/"] {
        assert!(!code.contains(forbidden), "cmd/background.rs mentions {forbidden}");
    }
}

/// `ddc disk` and `ddc disk agents` only measure (story "Whole disk: what
/// lies outside the home folder"): its sources reach no cleanup, start no program themselves, and
/// never run `xcrun` or a first launch, which can ask for a password.
#[test]
fn the_disk_report_never_reaches_cleanup() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = vec![
        root.join("ddc-cli/src/cmd/disk.rs"),
        root.join("ddc-cli/src/cmd/disk_agents.rs"),
        root.join("ddc-cli/src/cmd/disk_brew_packages.rs"),
    ];
    // `ddc disk agents` too (story "Coding agents in the Whole disk view").
    for dir in ["ddc-core/src/disk", "ddc-core/src/agents"] {
        for entry in std::fs::read_dir(root.join(dir)).unwrap() {
            files.push(entry.unwrap().path());
        }
    }
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        let code: String = text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        for forbidden in [
            "Executor",
            "build_plan",
            "plan::",
            "trash",
            "Consent",
            "remove_",
            "Command::new",
            "xcrun",
            "runFirstLaunch",
            "\"delete\"",
        ] {
            assert!(!code.contains(forbidden), "{} mentions {forbidden}", file.display());
        }
    }
}

/// `ddc disk free` runs only the commands the safety ADR names (amendments
/// of 2026-10-28 and 2026-10-29): no item cleanup, no program started by
/// hand, no `xcrun` or first launch, no `simctl runtime delete
/// --keep-asset`, no `--prune`, no `sudo`, and no Homebrew `autoremove`.
#[test]
fn disk_free_runs_only_its_tools_commands() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut files = vec![root.join("ddc-cli/src/cmd/disk_free.rs")];
    for entry in std::fs::read_dir(root.join("ddc-core/src/free")).unwrap() {
        files.push(entry.unwrap().path());
    }
    for file in files {
        let text = std::fs::read_to_string(&file).unwrap();
        for forbidden in [
            "Executor",
            "build_plan",
            "trash",
            "Consent",
            "remove_",
            "Command::new",
            "xcrun",
            "runFirstLaunch",
            "keep-asset",
            "prune",
            "sudo",
            "\"autoremove\"",
        ] {
            assert!(!text.contains(forbidden), "{} mentions {forbidden}", file.display());
        }
    }
}

/// brew runs only from the Homebrew area's `brew`, with the commands the
/// safety ADR's amendment of 2026-10-29 names: the listing reads (`info`,
/// `services list`) and `uninstall --formula` by full name, `--force` only to
/// take every keg. Never a cask, `--zap`, `--ignore-dependencies`,
/// `autoremove` or `sudo`, and the listing removes nothing.
#[test]
fn brew_runs_only_its_named_commands() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut reads = vec![root.join("ddc-cli/src/cmd/disk_brew_packages.rs")];
    for entry in std::fs::read_dir(root.join("ddc-core/src/brew")).unwrap() {
        reads.push(entry.unwrap().path());
    }
    for file in &reads {
        let text = std::fs::read_to_string(file).unwrap();
        for forbidden in [
            "\"uninstall\"",
            "\"--force\"",
            "\"--zap\"",
            "\"--cask\"",
            "\"--ignore-dependencies\"",
            "\"autoremove\"",
            "sudo",
            "Command::new",
            "remove_",
            "trash",
        ] {
            assert!(!text.contains(forbidden), "{} mentions {forbidden}", file.display());
        }
    }
    for entry in std::fs::read_dir(root.join("ddc-core/src/free")).unwrap() {
        let file = entry.unwrap().path();
        let text = std::fs::read_to_string(&file).unwrap();
        for forbidden in ["--zap", "--cask", "--ignore-dependencies", "find(\"brew\")"] {
            assert!(!text.contains(forbidden), "{} mentions {forbidden}", file.display());
        }
    }
    assert_eq!(ddc_core::brew::INFO, ["info", "--json=v2", "--installed"]);
    assert_eq!(ddc_core::brew::SERVICES_LIST, ["services", "list", "--json"]);
    assert_eq!(ddc_core::free::uninstall::UNINSTALL, ["uninstall", "--formula"]);
    assert_eq!(ddc_core::free::uninstall::FORCE, "--force");
    assert_eq!(
        ddc_core::brew::ENV,
        [
            ("HOMEBREW_NO_AUTOREMOVE", "1"),
            ("HOMEBREW_NO_ENV_HINTS", "1"),
            ("NO_COLOR", "1"),
            ("HOMEBREW_NO_AUTO_UPDATE", "1")
        ]
    );
}

/// conda runs only as an install's own `bin/conda`, and only with the three
/// argument lists the safety ADR's amendment of 2026-10-29 names: `env
/// remove` by path, never by name or of the base; `clean` limited to one
/// package folder, never `--all`, `--tempfiles` or `--force-pkgs-dirs`.
#[test]
fn conda_runs_only_its_named_commands() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let files = [root.join("ddc-core/src/conda.rs"), root.join("ddc-core/src/free/conda.rs")];
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap();
        for forbidden in [
            "\"-n\"",
            "\"--name\"",
            "\"--all\"",
            "\"--tempfiles\"",
            "\"--force-pkgs-dirs\"",
            "\"base\"",
            "find(\"conda\")",
            "Command::new",
            "sudo",
        ] {
            assert!(!text.contains(forbidden), "{} mentions {forbidden}", file.display());
        }
    }
    for entry in std::fs::read_dir(root.join("ddc-core/src/free")).unwrap() {
        let file = entry.unwrap().path();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(!text.contains("find(\"conda\")"), "{} looks conda up on the tool path", file.display());
    }
    assert_eq!(ddc_core::conda::ENV_REMOVE, ["env", "remove", "-p", "{path}", "--yes"]);
    assert_eq!(ddc_core::conda::CLEAN, ["clean", "--index-cache", "--packages", "--tarballs", "--logfiles", "--yes"]);
    assert_eq!(
        ddc_core::conda::CLEAN_DRY_RUN,
        ["clean", "--index-cache", "--packages", "--tarballs", "--logfiles", "--dry-run", "--json"]
    );
    assert_eq!(ddc_core::conda::PKGS_DIRS_VAR, "CONDA_PKGS_DIRS");
}

/// The cost counters of the complexity guard (story "Complexity guard:
/// operation counts and scaling exponents") never reach a release: the dist
/// and app builds pass no features, and no manifest turns `op-stats` on
/// outside a `[features]` table or as a default. A default reached through
/// another feature is caught by `nm`, which `make release-check` and both
/// build scripts run on the release binary to look for the counters' marker.
/// The dist build is `scripts/build-cli.sh`, which `scripts/build-dist.sh` and
/// CI share (story "Build matrix: every target built, tested and packaged
/// from one tag", DDC-007).
#[test]
fn the_release_build_carries_no_counters() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let marker = "DDC_OP_STATS";
    let op_stats = std::fs::read_to_string(root.join("crates/ddc-core/src/platform/op_stats.rs")).unwrap();
    assert!(op_stats.contains(&format!("pub static {marker}:")), "platform/op_stats.rs no longer exports {marker}");
    let dist = std::fs::read_to_string(root.join("scripts/build-dist.sh")).unwrap();
    assert!(
        dist.contains("scripts/build-cli.sh\" \"$TARGET\""),
        "scripts/build-dist.sh no longer builds with scripts/build-cli.sh"
    );
    for script in ["scripts/build-cli.sh", "scripts/build-app.sh"] {
        let text = std::fs::read_to_string(root.join(script)).unwrap().replace("\\\n", " ");
        assert!(text.contains(&format!("grep {marker} ")), "{script} does not look for {marker}");
        assert!(text.contains("grep ' _main$'"), "{script} does not check that nm sees a symbol table");
        let build: Vec<&str> = text.lines().filter(|l| l.contains("\"$CARGO\" build")).collect();
        assert_eq!(build.len(), 1, "{script}: {build:?}");
        for word in build[0].split_whitespace() {
            assert!(
                !word.starts_with("-F") && !word.starts_with("--features") && word != "--all-features",
                "{script}: {}",
                build[0]
            );
        }
    }
    let makefile = std::fs::read_to_string(root.join("Makefile")).unwrap();
    assert!(makefile.contains(&format!("grep {marker} ")), "Makefile: release-check does not look for {marker}");
    assert!(makefile.contains("grep ' _main$$'"), "Makefile: release-check does not check that nm sees a symbol table");
    let mut manifests = vec![root.join("Cargo.toml")];
    for entry in std::fs::read_dir(root.join("crates")).unwrap() {
        let manifest = entry.unwrap().path().join("Cargo.toml");
        if manifest.exists() {
            manifests.push(manifest);
        }
    }
    for manifest in manifests {
        let text = std::fs::read_to_string(&manifest).unwrap();
        let (mut section, mut key) = ("", "");
        for line in text.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
            if line.starts_with('[') && !line.starts_with("[\"") {
                section = line;
                continue;
            }
            if let Some((k, _)) = line.split_once('=') {
                key = k.trim();
            }
            if line.contains("op-stats") {
                assert_eq!(section, "[features]", "{}: {line}", manifest.display());
                assert_ne!(key, "default", "{}: {line}", manifest.display());
            }
        }
    }
}

/// Every directory read in the core is counted: it goes through
/// `stats::read_dir` or a `DirReader`, never `fs::read_dir` or
/// `Path::read_dir` directly, by any spelling.
#[test]
fn the_core_reads_directories_only_through_the_counters() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ddc-core/src");
    let stats = src.join("stats.rs");
    let mut dirs = vec![src];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") || path == stats {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for line in text.lines().filter(|l| !l.trim_start().starts_with("//")) {
                assert!(!line.replace("stats::read_dir", "").contains("read_dir"), "{}: {line}", path.display());
            }
        }
    }
}

/// The simulated unported build stays a test build (ADR "Platform seam: one
/// platform module per OS with declared capabilities", §8): the feature
/// `simulate-unported` is declared in a `[features]` table, enabled by no
/// dependency and no default, and passed by no build script, hook or
/// workflow; in the Makefile only `unported-check`, which builds into its
/// own target folder, passes it.
#[test]
fn only_the_unported_check_builds_the_simulated_platform() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let feature = "simulate-unported";
    let mut manifests = vec![root.join("Cargo.toml"), root.join("fuzz/Cargo.toml")];
    for entry in std::fs::read_dir(root.join("crates")).unwrap() {
        let manifest = entry.unwrap().path().join("Cargo.toml");
        if manifest.exists() {
            manifests.push(manifest);
        }
    }
    for manifest in manifests {
        let text = std::fs::read_to_string(&manifest).unwrap();
        let mut section = "";
        for line in text.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
            if line.starts_with('[') && !line.starts_with("[\"") {
                section = line;
                continue;
            }
            if line.contains(feature) {
                assert_eq!(section, "[features]", "{}: {line}", manifest.display());
                let key = line.split_once('=').map_or("", |(k, _)| k.trim());
                assert_eq!(key, feature, "{}: only the feature itself forwards it: {line}", manifest.display());
            }
        }
    }
    let mut scripts: Vec<_> = ["scripts", ".github/workflows", ".githooks"]
        .iter()
        .filter_map(|dir| std::fs::read_dir(root.join(dir)).ok())
        .flat_map(|entries| entries.map(|e| e.unwrap().path()))
        .filter(|p| p.is_file())
        .collect();
    scripts.sort();
    assert!(scripts.iter().any(|p| p.ends_with("scripts/build-cli.sh")), "{scripts:?}");
    for script in scripts {
        let text = std::fs::read_to_string(&script).unwrap();
        assert!(!text.contains(feature), "{} passes {feature}", script.display());
    }
    let makefile = std::fs::read_to_string(root.join("Makefile")).unwrap();
    assert!(makefile.contains("UNPORTED := CARGO_TARGET_DIR=target/unported\n"), "Makefile: no own target folder");
    for line in makefile.lines().filter(|l| l.contains(feature) && !l.starts_with('#')) {
        assert!(line.starts_with("\t$(UNPORTED) $(CARGO) "), "Makefile: {line}");
    }
}

/// The seam's switches are literals (ADR "Platform seam: one platform module
/// per OS with declared capabilities", §5, §8): `platform/mod.rs` and
/// `platform/types.rs` read no environment variable, and the only
/// `cfg!(feature …)` of `mod.rs` is `simulate-unported`; each OS module's
/// `SUPPORTED` is `true` or `false` and its `CAPABILITIES` a slice written
/// out. Every other file under `platform/` reads the environment only for
/// the folders of its user, from the allowlist, and only a `folders` item
/// reads `HOME`.
#[test]
fn the_platform_switches_are_literals() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ddc-core/src/platform");
    let code = |path: &Path| -> String {
        let text = std::fs::read_to_string(path).unwrap();
        text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
    };
    let env_reads = ["env::var(", "var_os(", "option_env!(", "env!("];
    let shared = code(&dir.join("mod.rs"));
    let features: Vec<&str> = shared.match_indices("feature").map(|(i, _)| &shared[i..]).collect();
    assert!(!features.is_empty(), "platform/mod.rs: the simulate-unported switch is gone");
    for at in features {
        assert!(at.starts_with("feature = \"simulate-unported\")"), "platform/mod.rs: {}", at.lines().next().unwrap());
    }
    for file in ["macos/mod.rs", "linux/mod.rs", "windows.rs"] {
        let text = code(&dir.join(file));
        let supported: Vec<&str> = text.lines().filter(|l| l.contains("const SUPPORTED")).collect();
        assert!(
            supported == ["pub const SUPPORTED: bool = true;"] || supported == ["pub const SUPPORTED: bool = false;"],
            "{file}: {supported:?}"
        );
        let start = text.find("pub const CAPABILITIES: &[Capability] = &[").unwrap_or_else(|| panic!("{file}"));
        let list = &text[start..];
        let list = &list[list.find("= &[").unwrap() + 4..list.find("];").unwrap()];
        for item in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let name = item.strip_prefix("Capability::").unwrap_or_else(|| panic!("{file}: {item}"));
            assert!(name.chars().all(char::is_alphanumeric), "{file}: {item}");
        }
    }
    let allowed = ["HOME", "USERPROFILE", "HOMEDRIVE", "HOMEPATH", "LOCALAPPDATA"];
    let files = rust_files(&dir);
    assert!(files.len() > 4, "{files:?}");
    for path in files {
        let rel = path.strip_prefix(&dir).unwrap().to_string_lossy().into_owned();
        let text = code(&path);
        let reads_nothing = rel == "mod.rs" || rel == "types.rs";
        let folders = path.file_stem().is_some_and(|s| s == "folders");
        for read in env_reads {
            for (i, _) in text.match_indices(read) {
                assert!(!reads_nothing, "platform/{rel} reads the environment: {read}");
                let name = text[i + read.len()..].trim_start_matches(['(', '"']).split('"').next().unwrap();
                assert!(
                    allowed.contains(&name) || name.starts_with("XDG_"),
                    "platform/{rel} reads {name} from the environment"
                );
                assert!(name != "HOME" || folders, "platform/{rel} reads HOME outside the folders item");
            }
        }
    }
}

/// `ddc keep remove` removes one `.ddc-keep` and nothing else (safety ADR,
/// amendment of 2026-10-30): its code names no removal, trash, program or
/// executor, and `DirFd::unlink_child`, the only removal outside the
/// executor, has one caller in every crate's sources, with `KEEP_MARKER`.
#[test]
fn keep_remove_unlinks_only_its_marker() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    for file in ["ddc-cli/src/cmd/keep.rs", "ddc-core/src/keep.rs"] {
        let text = code(&crates.join(file));
        for forbidden in [
            "remove_dir",
            "remove_file",
            "trash",
            "Trash",
            "Command::new",
            "process::Command",
            "Executor",
            "execute",
            "exec::delete",
            "build_plan",
            "plan::",
            "Consent",
            "rename_child",
        ] {
            assert!(!text.contains(forbidden), "{file} mentions {forbidden}");
        }
    }
    let mut calls = Vec::new();
    for name in ["ddc-cli", "ddc-core", "ddc-loadgen"] {
        let src = crates.join(name).join("src");
        let platform = src.join("platform");
        for path in rust_files(&src).into_iter().filter(|p| !p.starts_with(&platform)) {
            let text = code(&path);
            for args in arguments(&text, "unlink_child(") {
                let rel = path.strip_prefix(&crates).unwrap().to_string_lossy().replace('\\', "/");
                calls.push((rel, args.to_owned()));
            }
        }
    }
    assert_eq!(calls, [("ddc-core/src/keep.rs".to_owned(), "KEEP_MARKER.as_ref()".to_owned())]);
}

/// Every `.rs` file under `dir`, at any depth, sorted.
fn rust_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let (mut files, mut dirs) = (Vec::new(), vec![dir.to_path_buf()]);
    while let Some(d) = dirs.pop() {
        for entry in std::fs::read_dir(&d).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// No OS outside the platform folders (story "Move OS calls behind the
/// platform module", AC3; ADR "Platform seam: one platform module per OS
/// with declared capabilities", §1, §2): no file under a crate's `src/`
/// outside `ddc-core/src/platform/`, `ddc-cli/src/platform/` and
/// `ddc-loadgen/src/platform/` names `libc`, `rustix`, `windows_sys`,
/// `windows::` or `std::os` (any submodule), or puts a `cfg`, `cfg!` or
/// `cfg_attr` on `target_os`, `target_family`, `unix` or `windows`. Neither
/// the CLI outside its folder nor the load generator names the core's macOS
/// module, by its old path or its new one.
#[test]
fn no_file_outside_the_platform_folders_names_an_os() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut names = Vec::new();
    let mut checked = 0;
    for entry in std::fs::read_dir(&crates).unwrap() {
        let krate = entry.unwrap().path();
        let src = krate.join("src");
        if !src.is_dir() {
            continue;
        }
        let name = krate.file_name().unwrap().to_string_lossy().into_owned();
        let platform = src.join("platform");
        for path in rust_files(&src).into_iter().filter(|p| !p.starts_with(&platform)) {
            let rel = format!("{name}/src/{}", path.strip_prefix(&src).unwrap().display());
            let text = code(&path);
            let words = os_words(&text);
            assert!(words.is_empty(), "{rel} names an OS outside its platform folder: {words:?}");
            checked += 1;
        }
        names.push(name);
    }
    names.sort();
    assert_eq!(names, ["ddc-cli", "ddc-core", "ddc-loadgen"]);
    assert!(checked > 50, "only {checked} files read");
    // The core's macOS module, in every file of the CLI outside its folder
    // (its tests too) and in every file of the load generator. This file
    // names the paths it looks for.
    for (name, own) in [("ddc-cli", Some("src/platform")), ("ddc-loadgen", None)] {
        let krate = crates.join(name);
        let own = own.map(|o| krate.join(o));
        for path in rust_files(&krate) {
            if own.as_ref().is_some_and(|o| path.starts_with(o)) || path.ends_with("tests/structure.rs") {
                continue;
            }
            let text = code(&path);
            for core_os in ["ddc_core::sys::", "ddc_core::platform::macos::"] {
                assert!(!text.contains(core_os), "{} names the core's macOS module: {core_os}", path.display());
            }
        }
    }
}

/// What of `text` names an OS: a crate or module of one, or a `cfg` on one.
fn os_words(text: &str) -> Vec<String> {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let after_boundary = |i: usize| !text[..i].ends_with(ident);
    let mut found = Vec::new();
    for word in ["libc", "rustix", "windows_sys"] {
        for (i, _) in text.match_indices(word) {
            if after_boundary(i) && !text[i + word.len()..].starts_with(ident) {
                found.push(word.to_owned());
            }
        }
    }
    for path in
        ["windows::", "std::os", "os::unix", "os::macos", "os::linux", "os::windows", "os::fd", "os::raw", "os::darwin"]
    {
        if text.match_indices(path).any(|(i, _)| after_boundary(i)) {
            found.push(path.to_owned());
        }
    }
    for open in ["cfg(", "cfg!(", "cfg_attr("] {
        for predicate in arguments(text, open) {
            if words(predicate).any(|w| ["target_os", "target_family", "unix", "windows"].contains(&w)) {
                found.push(format!("{open}{predicate})"));
            }
        }
    }
    found
}

/// What follows each `open` (a name and its `(`, not the end of a longer
/// name) up to its matching `)`.
fn arguments<'a>(text: &'a str, open: &str) -> Vec<&'a str> {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut found = Vec::new();
    for (i, _) in text.match_indices(open).filter(|&(i, _)| !text[..i].ends_with(ident)) {
        let start = i + open.len();
        let mut depth = 1;
        let end = text[start..]
            .char_indices()
            .find(|&(_, c)| {
                depth += match c {
                    '(' => 1,
                    ')' => -1,
                    _ => 0,
                };
                depth == 0
            })
            .map_or(text.len(), |(j, _)| start + j);
        found.push(&text[start..end]);
    }
    found
}

/// The identifiers of `text`.
fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|w| !w.is_empty())
}

/// `unsafe_code` is denied everywhere (story "Move OS calls behind the
/// platform module", AC4): the workspace lints deny it, every crate inherits
/// them, and only the OS modules `ddc-core/src/platform/macos/mod.rs` and
/// `ddc-core/src/platform/linux/mod.rs` (seam ADR §2, amended 2026-10-31)
/// and `ddc-core/src/platform/op_stats.rs` allow it, so no test does.
#[test]
fn only_the_os_modules_and_the_counters_allow_unsafe_code() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let lint = ["unsafe", "_code"].concat();
    let workspace = std::fs::read_to_string(root.join("Cargo.toml")).unwrap();
    let rust_lints = &workspace[workspace.find("[workspace.lints.rust]").expect("no [workspace.lints.rust]")..];
    let rust_lints = &rust_lints[..rust_lints[1..].find("\n[").map_or(rust_lints.len(), |i| i + 1)];
    assert!(rust_lints.lines().any(|l| l.trim() == format!("{lint} = \"deny\"")), "{rust_lints}");
    let crates = root.join("crates");
    let mut allowed = Vec::new();
    for entry in std::fs::read_dir(&crates).unwrap() {
        let krate = entry.unwrap().path();
        let Ok(manifest) = std::fs::read_to_string(krate.join("Cargo.toml")) else { continue };
        assert!(manifest.contains("\n[lints]\nworkspace = true\n"), "{}: does not inherit the lints", krate.display());
        for path in rust_files(&krate).into_iter().filter(|p| !p.components().any(|c| c.as_os_str() == "target")) {
            let text = code(&path);
            let mut levels = ["allow(", "expect(", "warn("].into_iter().flat_map(|open| arguments(&text, open));
            if levels.any(|args| words(args).any(|w| w == lint)) {
                allowed.push(path.strip_prefix(&crates).unwrap().to_string_lossy().replace('\\', "/"));
            }
        }
    }
    allowed.sort();
    assert_eq!(
        allowed,
        [
            "ddc-core/src/platform/linux/mod.rs",
            "ddc-core/src/platform/macos/mod.rs",
            "ddc-core/src/platform/op_stats.rs"
        ]
    );
}

/// The code of a Rust file: every line but `//` comments.
fn code(path: &Path) -> String {
    let text = std::fs::read_to_string(path).unwrap();
    text.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
}

/// Every skip names its own test (ADR "Platform seam: one platform module
/// per OS with declared capabilities", §9, amended; story "Build matrix:
/// every target built, tested and packaged from one tag", stage 2). The
/// native jobs compare the `skipped <label>:` lines with a committed list
/// (`.config/native-skips/`), so a label that is not the test's fn name, a
/// stale one or one copied from a sibling, could hide a new skip behind an
/// old line. A label is a literal equal to the enclosing fn's name,
/// `stringify!` of a macro's test name, or the `test` parameter of a helper
/// in the same file whose own callers this checks the same way. A file's
/// own closure, parameter or fn of the same name that takes no `test` is
/// not a skip. A skip line is printed only by such a helper, as
/// `skipped {test}: `, never by hand.
#[test]
fn every_skip_names_its_own_test() {
    const SKIPS: [&str; 7] =
        ["unsupported", "lacks", "no_rule", "skipped", "skipped_without", "skip_unported", "idle_share"];
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let (mut checked, mut wrong) = (0, Vec::new());
    for entry in std::fs::read_dir(&crates).unwrap() {
        let krate = entry.unwrap().path();
        for path in rust_files(&krate).into_iter().filter(|p| !p.components().any(|c| c.as_os_str() == "target")) {
            let text = code(&path);
            let fns = fn_starts(&text);
            let enclosing = |at: usize| fns.iter().rev().find(|f| f.0 < at);
            // A file's own `skipped` closure or fn of another kind is not a skip.
            let shadowed = |name: &str| {
                ["let {} =", "({}: ", ", {}: "].iter().any(|form| text.contains(&form.replace("{}", name)))
                    || fns.iter().any(|&(_, f, takes_test)| f == name && !takes_test)
            };
            let mut names: Vec<&str> = SKIPS.into_iter().filter(|n| !shadowed(n)).collect();
            // The file's own helpers that pass their `test` on, to a fixpoint.
            loop {
                let forwarders: Vec<&str> = calls(&text, &names)
                    .filter(|(_, arg)| *arg == "test")
                    .filter_map(|(at, _)| enclosing(at).filter(|f| f.2).map(|f| f.1))
                    .filter(|f| !names.contains(f))
                    .collect();
                if forwarders.is_empty() {
                    break;
                }
                names.extend(forwarders);
                names.sort();
                names.dedup();
            }
            // A skip line printed by hand escapes both this check and the native
            // jobs' list: only a helper taking `test` prints one.
            for (at, _) in text.match_indices("eprintln!(\"skipped") {
                let helper =
                    text[at..].starts_with("eprintln!(\"skipped {test}: ") && enclosing(at).is_some_and(|f| f.2);
                if !helper {
                    wrong.push(format!("{}: a skip line printed outside a helper", path.display()));
                }
            }
            for (at, arg) in calls(&text, &names) {
                let Some(&(_, test, takes_test)) = enclosing(at) else {
                    wrong.push(format!("{}: {arg} outside any fn", path.display()));
                    continue;
                };
                let named = arg.strip_prefix('"').and_then(|a| a.strip_suffix('"')) == Some(test)
                    || arg == format!("stringify!({test})")
                    || (arg == "test" && takes_test);
                if !named {
                    wrong.push(format!("{}: {arg} in fn {test}", path.display()));
                }
                checked += 1;
            }
        }
    }
    assert!(wrong.is_empty(), "skip labels that are not their test's name:\n{}", wrong.join("\n"));
    assert!(checked > 300, "only {checked} skips read");
}

/// Each `fn` of `text`: where it starts, its name (a macro's `$test` too),
/// and whether it takes a `test: &str`.
fn fn_starts(text: &str) -> Vec<(usize, &str, bool)> {
    let ident = |c: char| c.is_alphanumeric() || c == '_' || c == '$';
    text.match_indices("fn ")
        .filter(|&(i, _)| !text[..i].ends_with(ident))
        .filter_map(|(i, _)| {
            let rest = &text[i + 3..];
            let name = &rest[..rest.find(|c: char| !ident(c)).unwrap_or(rest.len())];
            let params = rest[name.len()..].split(')').next().unwrap_or("");
            (!name.is_empty()).then_some((i, name, params.contains("test: &str")))
        })
        .collect()
}

/// Each call of one of `names` in `text`, not its definition: where it is,
/// and its first argument, trimmed. Calls without arguments are someone
/// else's (`unported.rs`'s `unsupported()`).
fn calls<'a>(text: &'a str, names: &[&str]) -> impl Iterator<Item = (usize, &'a str)> {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut found = Vec::new();
    for name in names {
        let open = format!("{name}(");
        for (i, _) in text.match_indices(&open) {
            if text[..i].ends_with(ident) || text[..i].trim_end().ends_with("fn") {
                continue;
            }
            let args = &text[i + open.len()..];
            let (mut depth, mut end) = (0i32, args.len());
            for (j, c) in args.char_indices() {
                match c {
                    '(' | '[' | '{' => depth += 1,
                    ')' | ']' | '}' if depth == 0 => {
                        end = j;
                        break;
                    }
                    ')' | ']' | '}' => depth -= 1,
                    ',' if depth == 0 => {
                        end = j;
                        break;
                    }
                    _ => {}
                }
            }
            let arg = args[..end].trim();
            if !arg.is_empty() {
                found.push((i, arg));
            }
        }
    }
    found.sort();
    found.into_iter()
}

/// Every path a JSON output writes goes through `json_path`, so a name that
/// is not UTF-8 is written lossily instead of failing the whole report (ADR
/// "Rust core library and CLI with JSON output", amendment of 2026-10-31).
/// `settings.json` is the exception: its paths come from JSON, so they are
/// UTF-8 already.
#[test]
fn every_serialized_path_is_written_through_json_path() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ddc-core/src");
    let mut bare = Vec::new();
    for path in rust_files(&src).into_iter().filter(|p| !p.ends_with("settings.rs")) {
        let text = std::fs::read_to_string(&path).unwrap();
        let (mut derive, mut serialized, mut depth, mut previous) = (false, false, 0i32, "");
        for line in text.lines() {
            let t = line.trim();
            if t.starts_with("#[derive(") {
                derive = t.contains("Serialize");
            }
            if depth == 0 && (t.contains("struct ") || t.contains("enum ")) && !t.starts_with("//") {
                serialized = derive;
                derive = false;
            }
            let field = t.starts_with("pub ")
                && ["PathBuf,", "Option<PathBuf>,", "Vec<PathBuf>,"].iter().any(|ty| t.ends_with(&format!(": {ty}")));
            if serialized && depth >= 1 && field && !previous.contains("json_path") {
                bare.push(format!("{}: {t}", path.display()));
            }
            depth += line.matches('{').count() as i32 - line.matches('}').count() as i32;
            if depth == 0 {
                serialized = false;
            }
            previous = t;
        }
    }
    assert!(bare.is_empty(), "serialized paths without json_path:\n{}", bare.join("\n"));
}
