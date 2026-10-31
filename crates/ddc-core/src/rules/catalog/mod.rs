//! The built-in rule table. Ids, classes and paths follow the tables in story
//! "Detection rule catalog" (epic DDC-001) and `bench/make_baseline.py`.
//! Order matters for specialized rules: the first match wins.
//!
//! One list per platform (story "Rule catalog split: shared and per-OS
//! sections", DDC-007): [`shared`] defines the rules every platform lists,
//! and `macos`, `linux` and `windows` list theirs in match order. The list
//! is chosen at run time from a [`Platform`] value, never by `cfg`, so every
//! build holds all three and `ddc rules --platform` can print any of them.
//! This module holds the builders and the helper lists.

mod linux;
mod macos;
mod shared;
// Never `windows::…`: that path names the OS crate, which the structure
// test keeps inside the platform folders.
mod windows;
use self::windows as win;

pub use shared::SHARED_IDS;

use super::{Clones, Matcher, Rule, Scope};
use crate::class::{
    CondaFolder, Fallback, Location, LocationCompare, LocationProbe, NativeCommand, ReadyProbe, Removal,
};
use crate::platform::{PLATFORM, Platform};

/// The rules of platform `p`, in match order. A list is a `static`, so a
/// rule's address is its identity ([`super::Catalog`] finds a rule's tag by
/// it).
pub const fn rules_of(p: Platform) -> &'static [Rule] {
    match p {
        Platform::Macos => &macos::RULES,
        Platform::Linux => &linux::RULES,
        Platform::Windows => &win::RULES,
    }
}

/// The rules of the platform this build is for: the same slice as
/// `rules_of(PLATFORM)`.
pub static RULES: &[Rule] = rules_of(PLATFORM);

/// Directories, relative to `$HOME`, that are never a finding nor removed
/// themselves on platform `p` (safety ADR "Cleanup safety model", §6,
/// amended 2026-10-30): the shared section and `p`'s own. `""` is the home.
pub const fn protected_in_home(p: Platform) -> &'static [&'static str] {
    match p {
        Platform::Macos => &macos::PROTECTED_IN_HOME,
        Platform::Linux => &linux::PROTECTED_IN_HOME,
        Platform::Windows => &win::PROTECTED_IN_HOME,
    }
}

/// System directories that are never removed on platform `p` (§6).
pub const fn protected_system(p: Platform) -> &'static [&'static str] {
    match p {
        Platform::Macos => &macos::PROTECTED_SYSTEM,
        Platform::Linux => &linux::PROTECTED_SYSTEM,
        Platform::Windows => &win::PROTECTED_SYSTEM,
    }
}

/// `a` then `b`, as one array of `N`: a section of protected folders after
/// the shared one. A wrong `N` fails the build.
const fn concat<const N: usize>(a: &[&'static str], b: &[&'static str]) -> [&'static str; N] {
    assert!(a.len() + b.len() == N, "the length is not the sum of the sections");
    let mut out = [""; N];
    let mut i = 0;
    while i < a.len() {
        out[i] = a[i];
        i += 1;
    }
    let mut j = 0;
    while j < b.len() {
        out[i + j] = b[j];
        j += 1;
    }
    out
}

const PY_LOCKS: &[&str] = &["uv.lock", "poetry.lock", "Pipfile.lock", "requirements*.txt"];
const JS_LOCKS: &[&str] = &["package-lock.json", "pnpm-lock.yaml", "yarn.lock", "bun.lock", "bun.lockb"];
const JS_BUILD: &[&str] = &[".next", ".nuxt", ".svelte-kit", ".turbo", ".parcel-cache", ".angular"];
const GRADLE: &[&str] = &["build.gradle", "build.gradle.kts"];
const GRADLE_ANY: &[&str] = &["build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts"];
const NONE: &[&str] = &[];
/// App bundles of the JetBrains IDEs; editions match too (`PyCharm CE`, `IntelliJ IDEA Ultimate`).
const JETBRAINS: &[&str] = &[
    "IntelliJ IDEA",
    "PyCharm",
    "WebStorm",
    "GoLand",
    "CLion",
    "RustRover",
    "DataGrip",
    "Rider",
    "PhpStorm",
    "RubyMine",
    "DataSpell",
    "Fleet",
];

const fn rule(
    id: &'static str,
    category: crate::class::Category,
    class: crate::class::RiskClass,
    removal: Removal,
    matcher: Matcher,
) -> Rule {
    Rule {
        id,
        category,
        class,
        removal,
        matcher,
        lockfiles: None,
        owners: &[],
        hint: "",
        listed: true,
        clones: Clones::Never,
        scope: Scope::Item,
        origin: None,
        listed_only: false,
    }
}

const fn name(names: &'static [&'static str], siblings: &'static [&'static str]) -> Matcher {
    Matcher::Name { names, siblings, children_any: NONE }
}

const fn native(
    program: &'static str,
    args: &'static [&'static str],
    probe: Option<Location>,
    fallback: Fallback,
) -> Removal {
    native_env(program, args, probe, fallback, &[])
}

/// [`native`] with environment variables for the command.
const fn native_env(
    program: &'static str,
    args: &'static [&'static str],
    probe: Option<Location>,
    fallback: Fallback,
    env: &'static [(&'static str, &'static str)],
) -> Removal {
    Removal::Native { cmd: NativeCommand { program, args, location: probe, ready: None, env }, fallback }
}

const fn probe(program: &'static str, args: &'static [&'static str], compare: LocationCompare) -> Option<Location> {
    Some(Location::Probe(LocationProbe { program, args, compare }))
}

/// A command of the conda install that claims the finding (safety ADR,
/// amendment of 2026-10-29), never of a `conda` on the tool path.
const fn conda(
    args: &'static [&'static str],
    folder: CondaFolder,
    fallback: Fallback,
    env: &'static [(&'static str, &'static str)],
) -> Removal {
    Removal::Native {
        cmd: NativeCommand { program: "conda", args, location: Some(Location::Conda(folder)), ready: None, env },
        fallback,
    }
}

/// What the confirmation says of a conda package cache.
pub const CONDA_CACHE_HINT: &str =
    "Removes unused packages only: packages environments use stay, so less may be freed than the folder's size";

/// Why Docker Desktop is not ready when it does not answer; starting it helps.
pub const DOCKER_DOWN: &str = "Docker Desktop is not running or not answering";

/// Docker Desktop's engine answers on its own context. The context's
/// metadata lives under the sha256 of its name, `desktop-linux`.
const DOCKER_READY: ReadyProbe = ReadyProbe {
    program: "docker",
    args: &["--context", "desktop-linux", "version", "--format", "{{.Server.Version}}"],
    process: "com.docker.backend",
    context_meta: "contexts/meta/fe9c6bd7a66301f49ca9b6a70b217107cd1284598bfc254700c989b916da791e/meta.json",
    down: DOCKER_DOWN,
    no_context: "the desktop-linux context does not exist",
    known: &[
        ("does not exist", "the desktop-linux context does not exist"),
        ("Cannot connect to the Docker daemon", DOCKER_DOWN),
    ],
};

const DOCKER_PRUNE: &[NativeCommand] = &[
    NativeCommand {
        program: "docker",
        args: &["--context", "desktop-linux", "builder", "prune", "-f"],
        location: None,
        ready: Some(DOCKER_READY),
        env: &[],
    },
    NativeCommand {
        program: "docker",
        args: &["--context", "desktop-linux", "image", "prune", "-f"],
        location: None,
        ready: Some(DOCKER_READY),
        env: &[],
    },
];
const PODMAN_PRUNE: &[NativeCommand] =
    &[NativeCommand { program: "podman", args: &["image", "prune", "-f"], location: None, ready: None, env: &[] }];

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Platform; 3] = [Platform::Macos, Platform::Linux, Platform::Windows];

    fn ids(p: Platform) -> Vec<&'static str> {
        rules_of(p).iter().map(|r| r.id).collect()
    }

    /// A rule's tag is its index, found by address: `RULES` is this
    /// platform's list itself, not a copy.
    #[test]
    fn rules_is_the_list_of_this_platform() {
        assert!(std::ptr::eq(RULES, rules_of(PLATFORM)));
        for p in ALL {
            assert!(std::ptr::eq(rules_of(p), rules_of(p)), "{p}: one list, not a copy per use");
            // Each list is a catalog: it holds `user.kept`.
            super::super::Catalog::from_rules(rules_of(p), std::path::Path::new("/home/example"));
        }
    }

    /// Every rule id is in exactly one section, or in the shared section with
    /// per-OS paths (story "Rule catalog split: shared and per-OS sections",
    /// AC2).
    #[test]
    fn every_rule_id_is_in_one_section_or_shared() {
        for p in ALL {
            let mut sorted = ids(p);
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), rules_of(p).len(), "{p}: an id repeats");
        }
        // Windows lists the shared section alone until story "Windows scanner
        // and rule catalog" adds its own rules.
        assert_eq!(ids(Platform::Windows), SHARED_IDS);
        // Every list holds every shared rule, in the macOS order, and ends
        // with the generic rule, which only claims what no other rule did.
        for p in ALL {
            let shared: Vec<_> = ids(p).into_iter().filter(|id| SHARED_IDS.contains(id)).collect();
            assert_eq!(shared, SHARED_IDS, "{p}: the shared rules, in the macOS order");
            assert_eq!(ids(p).last(), Some(&"generic.cachedir-tag"), "{p}: the generic rule is last");
        }
        // An id on more than one platform is defined in `shared.rs`, and each
        // list holds that definition resolved for its platform.
        for p in ALL {
            let resolved = shared::all(p);
            for rule in rules_of(p) {
                let on = ALL.iter().filter(|q| ids(**q).contains(&rule.id)).count();
                assert!(on == 1 || SHARED_IDS.contains(&rule.id), "{p}: {} is on {on} platforms, not shared", rule.id);
                if let Some(def) = resolved.iter().find(|d| d.id == rule.id) {
                    assert_eq!(
                        format!("{rule:?}"),
                        format!("{def:?}"),
                        "{p}: {} is not its shared definition",
                        rule.id
                    );
                }
            }
        }
        // A shared rule differs between platforms only in its matcher's paths.
        let [m, l, w] = ALL.map(shared::all);
        for (i, mac) in m.iter().enumerate() {
            for other in [&l[i], &w[i]] {
                assert_eq!(mac.id, other.id);
                let same = mac.category == other.category
                    && mac.class == other.class
                    && mac.removal == other.removal
                    && mac.lockfiles == other.lockfiles
                    && mac.owners == other.owners
                    && mac.hint == other.hint
                    && mac.listed == other.listed
                    && mac.clones == other.clones
                    && mac.scope == other.scope
                    && mac.origin == other.origin
                    && mac.listed_only == other.listed_only
                    && std::mem::discriminant(&mac.matcher) == std::mem::discriminant(&other.matcher);
                assert!(same, "{}: {mac:?} and {other:?} differ beyond the paths", mac.id);
            }
        }
    }

    /// The macOS protected folders are §6's list of the safety ADR "Cleanup
    /// safety model", and no folder sits in two sections (story "Rule catalog
    /// split: shared and per-OS sections", AC4).
    #[test]
    fn the_macos_protected_folders_are_the_safety_adrs_list() {
        use std::collections::BTreeSet;
        // §6: `/`, `/Users`, `$HOME`, `~/Documents`, …, then the folders
        // neither removed nor walked, `~/Library/Mobile Documents` and
        // `~/Library/CloudStorage`.
        let in_home = [
            "",
            "Documents",
            "Desktop",
            "Pictures",
            "Movies",
            "Music",
            "Downloads",
            "Projects",
            "Library",
            "Library/Caches",
            "Library/Application Support",
            "Library/Containers",
            "Library/Group Containers",
            ".config",
            ".ssh",
            ".gnupg",
            ".local",
            ".cache",
            "Library/Mobile Documents",
            "Library/CloudStorage",
        ];
        let system = ["/", "/Users", "/System", "/Applications", "/Library", "/usr", "/opt", "/opt/homebrew"];
        let set = |s: &[&'static str]| s.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(set(protected_in_home(Platform::Macos)), set(&in_home));
        assert_eq!(protected_in_home(Platform::Macos).len(), in_home.len(), "no folder twice");
        assert_eq!(set(protected_system(Platform::Macos)), set(&system));
        assert_eq!(protected_system(Platform::Macos).len(), system.len(), "no folder twice");

        let own: [(Platform, &[&str]); 3] = [
            (Platform::Macos, &macos::OWN_PROTECTED_IN_HOME),
            (Platform::Linux, &linux::OWN_PROTECTED_IN_HOME),
            (Platform::Windows, &win::OWN_PROTECTED_IN_HOME),
        ];
        let shared = set(&shared::PROTECTED_IN_HOME);
        assert_eq!(shared.len(), shared::PROTECTED_IN_HOME.len(), "no shared folder twice");
        for (p, own) in own {
            assert!(own.iter().all(|f| !shared.contains(f)), "{p}: a folder is in its section and the shared one");
            assert_eq!(set(own).len(), own.len(), "{p}: a folder twice in its section");
            assert_eq!(set(protected_in_home(p)), &shared | &set(own), "{p}: the shared section and its own");
            assert!(protected_in_home(p).iter().all(|f| !std::path::Path::new(f).has_root()), "{p}: relative");
            assert!(protected_system(p).iter().all(|f| std::path::Path::new(f).has_root()), "{p}: absolute");
        }
    }

    /// The flip guard (safety ADR "Cleanup safety model", amendment of
    /// 2026-10-30): a platform may be supported only where its system
    /// folders hold the filesystem root and its home folders hold more than
    /// the shared section.
    fn may_be_supported(p: Platform) -> bool {
        let is_root = |f: &&str| {
            let path = std::path::Path::new(f);
            path.has_root() && path.parent().is_none()
        };
        protected_system(p).iter().any(is_root) && protected_in_home(p).len() > shared::PROTECTED_IN_HOME.len()
    }

    /// The guard holds on every OS the tests run on, the required macOS job
    /// included: macOS and Linux have their protected folders, Windows not yet;
    /// and no platform module's `SUPPORTED` literal is `true` while the
    /// guard is false for it, so switching one on fails here first.
    #[test]
    fn a_platform_is_supported_only_with_its_own_protected_folders() {
        assert!(may_be_supported(Platform::Macos));
        assert!(may_be_supported(Platform::Linux));
        assert!(!may_be_supported(Platform::Windows));
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/platform");
        for (p, file) in
            [(Platform::Macos, "macos/mod.rs"), (Platform::Linux, "linux/mod.rs"), (Platform::Windows, "windows.rs")]
        {
            let text = std::fs::read_to_string(dir.join(file)).unwrap();
            let line =
                text.lines().find(|l| l.starts_with("pub const SUPPORTED: bool =")).unwrap_or_else(|| panic!("{file}"));
            let supported = line.trim_end_matches(';').ends_with("true");
            assert!(
                !supported || may_be_supported(p),
                "platform/{file} is supported, but {p}'s protected folders are the shared section alone"
            );
        }
        assert!(!crate::platform::SUPPORTED || may_be_supported(PLATFORM), "{PLATFORM} is supported");
    }
}
