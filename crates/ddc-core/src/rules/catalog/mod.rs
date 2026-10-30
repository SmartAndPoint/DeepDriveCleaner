//! The built-in rule table. Ids, classes and paths follow the tables in story
//! "Detection rule catalog" (epic DDC-001) and `bench/make_baseline.py`.
//! Order matters for specialized rules: the first match wins.
//!
//! This module holds the builders and the helper lists the rules share; the
//! rules themselves are in [`macos`].

mod macos;

pub use macos::RULES;

use super::{Clones, Matcher, Rule, Scope};
use crate::class::{
    CondaFolder, Fallback, Location, LocationCompare, LocationProbe, NativeCommand, ReadyProbe, Removal,
};

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
