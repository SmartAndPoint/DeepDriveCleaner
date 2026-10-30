//! Shared rules: ddc's own leftovers and the project artifacts, in match order.

use super::super::{GRADLE, GRADLE_ANY, JS_BUILD, JS_LOCKS, NONE, PY_LOCKS, conda, name, rule};
use crate::class::Category::*;
use crate::class::RiskClass::*;
use crate::class::{CondaFolder, Fallback, Removal};
use crate::rules::{Clones, Matcher, Rule, TOMBSTONE};

pub(crate) const DDC_TOMBSTONE: Rule = Rule {
    hint: "left by an interrupted ddc clean",
    ..rule(TOMBSTONE, ProjectArtifact, Safe, Removal::Delete, Matcher::NameContains(".ddc-deleting-"))
};

pub(crate) const NODE_GLOBAL_PACKAGES: Rule = Rule {
    hint: "global packages; managed by the package manager",
    listed: false,
    ..rule(
        "node.global-packages",
        ProjectArtifact,
        UserData,
        Removal::None,
        Matcher::Under {
            name: "node_modules",
            prefixes: &[".npm-global", ".config/yarn/global", ".bun/install/global", ".volta", ".fnm"],
        },
    )
};

pub(crate) const PYTHON_VENV: Rule = Rule {
    lockfiles: Some(PY_LOCKS),
    clones: Clones::UvVenv,
    hint: "recreate: uv sync / pip install -r requirements.txt",
    ..rule("python.venv", ProjectArtifact, Rebuildable, Removal::Delete, Matcher::ContainsFile("pyvenv.cfg"))
};

// Never an install's own prefix (the base environment): only a child of
// an `envs` folder, and the probe refuses any install's `root_prefix`.
pub(crate) const PYTHON_CONDA_ENV: Rule = Rule {
    hint: "recreate: conda create, or conda env create from its environment file",
    ..rule(
        "python.conda-env",
        Toolchain,
        Review,
        conda(&crate::conda::ENV_REMOVE, CondaFolder::Environment, Fallback::Trash, &[]),
        Matcher::ChildHolding {
            parents: &["miniconda3/envs", "anaconda3/envs", "miniforge3/envs", ".conda/envs"],
            file: "conda-meta/history",
        },
    )
};

pub(crate) const PYTHON_TOX: Rule = Rule {
    hint: "recreated by tox/nox on the next run",
    ..rule(
        "python.tox",
        ProjectArtifact,
        Safe,
        Removal::Delete,
        name(&[".tox", ".nox"], &["tox.ini", "noxfile.py", "pyproject.toml", "setup.cfg"]),
    )
};

pub(crate) const PYTHON_PYCACHE: Rule =
    rule("python.pycache", ProjectArtifact, Safe, Removal::Delete, name(&["__pycache__"], NONE));

pub(crate) const PYTHON_TOOL_CACHE: Rule = rule(
    "python.tool-cache",
    ProjectArtifact,
    Safe,
    Removal::Delete,
    name(&[".pytest_cache", ".mypy_cache", ".ruff_cache"], NONE),
);

pub(crate) const NODE_MODULES: Rule = Rule {
    lockfiles: Some(JS_LOCKS),
    clones: Clones::PnpmModules,
    hint: "recreate: npm ci / pnpm install / yarn install",
    ..rule(
        "node.node_modules",
        ProjectArtifact,
        Rebuildable,
        Removal::Delete,
        name(&["node_modules"], &["package.json"]),
    )
};

pub(crate) const NODE_MODULES_ORPHAN: Rule = Rule {
    clones: Clones::PnpmModules,
    hint: "no package.json next to it: origin unknown",
    ..rule("node.node_modules-orphan", ProjectArtifact, Review, Removal::Trash, name(&["node_modules"], NONE))
};

pub(crate) const JS_BUILD_CACHE: Rule = Rule {
    hint: "rebuilt by the next build",
    ..rule("js.build-cache", ProjectArtifact, Safe, Removal::Delete, name(JS_BUILD, &["package.json"]))
};

pub(crate) const RUST_TARGET: Rule = Rule {
    hint: "rebuilt by cargo build",
    ..rule("rust.target", ProjectArtifact, Rebuildable, Removal::Delete, name(&["target"], &["Cargo.toml"]))
};

pub(crate) const MAVEN_TARGET: Rule = Rule {
    hint: "rebuilt by mvn package",
    ..rule("maven.target", ProjectArtifact, Rebuildable, Removal::Delete, name(&["target"], &["pom.xml"]))
};

pub(crate) const GRADLE_BUILD: Rule = Rule {
    hint: "rebuilt by gradle build",
    ..rule(
        "gradle.build",
        ProjectArtifact,
        Rebuildable,
        Removal::Delete,
        Matcher::Name { names: &["build"], siblings: GRADLE, children_any: &["intermediates", "classes", "tmp"] },
    )
};

pub(crate) const GRADLE_PROJECT_CACHE: Rule =
    rule("gradle.project-cache", ProjectArtifact, Safe, Removal::Delete, name(&[".gradle"], GRADLE_ANY));

pub(crate) const CMAKE_BUILD: Rule = Rule {
    hint: "rebuilt by cmake --build",
    ..rule("cmake.build", ProjectArtifact, Rebuildable, Removal::Delete, Matcher::CmakeBuild)
};

pub(crate) const COCOAPODS_PODS: Rule = Rule {
    lockfiles: Some(&["Podfile.lock"]),
    hint: "recreate: pod install",
    ..rule("cocoapods.pods", ProjectArtifact, Rebuildable, Removal::Delete, name(&["Pods"], &["Podfile"]))
};
