//! Shared rules: package caches and test browsers, some with per-OS paths,
//! and the generic `CACHEDIR.TAG` rule.

use super::super::{CONDA_CACHE_HINT, conda, native, native_env, probe, rule};
use super::PerOs;
use crate::class::Category::*;
use crate::class::RiskClass::*;
use crate::class::{CondaFolder, Fallback, LocationCompare, Removal};
use crate::platform::Platform;
use crate::rules::{Clones, Matcher, Rule};

/// npm's `cache` setting: `~/.npm` on POSIX, `%LocalAppData%\npm-cache` on
/// Windows (docs.npmjs.com, "config", `cache`).
pub(crate) const fn cache_npm(p: Platform) -> Rule {
    let paths = PerOs {
        macos: &[".npm/_cacache", ".npm/_npx"],
        linux: &[".npm/_cacache", ".npm/_npx"],
        windows: &["AppData/Local/npm-cache/_cacache", "AppData/Local/npm-cache/_npx"],
    };
    rule("cache.npm", PackageCache, Safe, Removal::Delete, Matcher::KnownPaths(paths.of(p)))
}

/// Yarn 1's cache: `~/Library/Caches/Yarn` on macOS, `~/.cache/yarn` on
/// Linux, `%LOCALAPPDATA%\Yarn\Cache` on Windows (classic.yarnpkg.com,
/// `yarn cache dir`); Yarn 2+'s global cache is `cache` in its
/// `globalFolder`, `~/.yarn/berry`, `%LOCALAPPDATA%\Yarn\Berry` on Windows
/// (yarnpkg.com, ".yarnrc.yml", `globalFolder`).
pub(crate) const fn cache_yarn(p: Platform) -> Rule {
    let paths = PerOs {
        macos: &["Library/Caches/Yarn", ".yarn/berry/cache"],
        linux: &[".cache/yarn", ".yarn/berry/cache"],
        windows: &["AppData/Local/Yarn/Cache", "AppData/Local/Yarn/Berry/cache"],
    };
    rule("cache.yarn", PackageCache, Safe, Removal::Delete, Matcher::KnownPaths(paths.of(p)))
}

pub(crate) const CACHE_BUN: Rule = Rule {
    clones: Clones::Always,
    ..rule("cache.bun", PackageCache, Safe, Removal::Delete, Matcher::KnownPaths(&[".bun/install/cache"]))
};

/// pnpm's `storeDir`: `~/Library/pnpm/store` on macOS,
/// `~/.local/share/pnpm/store` on Linux, `~/AppData/Local/pnpm/store` on
/// Windows (pnpm.io, "Settings", `storeDir`); `.pnpm-store` at the root of
/// another volume, here the home.
pub(crate) const fn cache_pnpm(p: Platform) -> Rule {
    let paths = PerOs {
        macos: &["Library/pnpm/store", ".pnpm-store"],
        linux: &[".local/share/pnpm/store", ".pnpm-store"],
        windows: &["AppData/Local/pnpm/store", ".pnpm-store"],
    };
    Rule {
        clones: Clones::Always,
        hint: "node_modules installed by pnpm are clones and stay intact",
        ..rule("cache.pnpm", PackageCache, Safe, Removal::Delete, Matcher::KnownPaths(paths.of(p)))
    }
}

/// uv's cache: `~/.cache/uv` on macOS and Linux, `%LOCALAPPDATA%\uv\cache`
/// on Windows (docs.astral.sh, "Caching", "Cache directory").
pub(crate) const fn cache_uv(p: Platform) -> Rule {
    let paths = PerOs { macos: &[".cache/uv"], linux: &[".cache/uv"], windows: &["AppData/Local/uv/cache"] };
    Rule {
        clones: Clones::Always,
        ..rule(
            "cache.uv",
            PackageCache,
            Safe,
            // A lock taken after the in-use check fails in seconds, not at
            // NATIVE_TIMEOUT (uv 0.9.16 and later; an older uv ignores it).
            native_env(
                "uv",
                &["cache", "clean"],
                probe("uv", &["cache", "dir"], LocationCompare::Equals),
                Fallback::Delete,
                &[("UV_LOCK_TIMEOUT", "10")],
            ),
            Matcher::KnownPaths(paths.of(p)),
        )
    }
}

// Only conda cleans it: environments may link to its packages, so it is
// never deleted or moved to the Trash without conda.
pub(crate) const CACHE_CONDA: Rule = Rule {
    hint: CONDA_CACHE_HINT,
    ..rule(
        "cache.conda",
        PackageCache,
        Safe,
        conda(
            &crate::conda::CLEAN,
            CondaFolder::PackageCache,
            Fallback::Skip,
            &[(crate::conda::PKGS_DIRS_VAR, "{path}")],
        ),
        Matcher::KnownPaths(&["miniconda3/pkgs", "anaconda3/pkgs", "miniforge3/pkgs", ".conda/pkgs"]),
    )
};

/// pip's cache: `~/Library/Caches/pip` on macOS, `~/.cache/pip` on Linux,
/// `%LocalAppData%\pip\Cache` on Windows (pip.pypa.io, "Caching", "Default
/// paths").
pub(crate) const fn cache_pip(p: Platform) -> Rule {
    let paths = PerOs { macos: &["Library/Caches/pip"], linux: &[".cache/pip"], windows: &["AppData/Local/pip/Cache"] };
    rule("cache.pip", PackageCache, Safe, Removal::Delete, Matcher::KnownPaths(paths.of(p)))
}

/// Poetry's `cache-dir`: `~/Library/Caches/pypoetry` on macOS,
/// `~/.cache/pypoetry` on Linux, `%LOCALAPPDATA%\pypoetry\Cache` on Windows
/// (python-poetry.org, "Configuration", `cache-dir`).
pub(crate) const fn cache_poetry(p: Platform) -> Rule {
    let paths = PerOs {
        macos: &["Library/Caches/pypoetry/cache", "Library/Caches/pypoetry/artifacts"],
        linux: &[".cache/pypoetry/cache", ".cache/pypoetry/artifacts"],
        windows: &["AppData/Local/pypoetry/Cache/cache", "AppData/Local/pypoetry/Cache/artifacts"],
    };
    rule("cache.poetry", PackageCache, Safe, Removal::Delete, Matcher::KnownPaths(paths.of(p)))
}

/// `GOCACHE`: `go-build` in the user cache folder, `~/Library/Caches` on
/// macOS, `~/.cache` on Linux, `%LocalAppData%` on Windows (pkg.go.dev,
/// "cmd/go", "Build and test caching"; `os.UserCacheDir`).
pub(crate) const fn cache_go_build(p: Platform) -> Rule {
    let paths = PerOs {
        macos: &["Library/Caches/go-build"],
        linux: &[".cache/go-build"],
        windows: &["AppData/Local/go-build"],
    };
    rule(
        "cache.go-build",
        PackageCache,
        Safe,
        native("go", &["clean", "-cache"], probe("go", &["env", "GOCACHE"], LocationCompare::Equals), Fallback::Delete),
        Matcher::KnownPaths(paths.of(p)),
    )
}

pub(crate) const CACHE_GO_MOD: Rule = rule(
    "cache.go-mod",
    PackageCache,
    Safe,
    native(
        "go",
        &["clean", "-modcache"],
        probe("go", &["env", "GOMODCACHE"], LocationCompare::Equals),
        Fallback::DeleteWritable,
    ),
    Matcher::KnownPaths(&["go/pkg/mod"]),
);

pub(crate) const CACHE_CARGO: Rule = rule(
    "cache.cargo",
    PackageCache,
    Safe,
    Removal::Delete,
    Matcher::KnownPaths(&[".cargo/registry/cache", ".cargo/registry/src", ".cargo/git/checkouts"]),
);

pub(crate) const CACHE_GRADLE: Rule =
    rule("cache.gradle", PackageCache, Safe, Removal::Delete, Matcher::KnownPaths(&[".gradle/caches"]));

pub(crate) const REPO_MAVEN: Rule = Rule {
    hint: "may contain artifacts installed locally with mvn install",
    ..rule("repo.maven", PackageCache, Review, Removal::Trash, Matcher::KnownPaths(&[".m2/repository"]))
};

/// Playwright's browsers: `~/Library/Caches/ms-playwright` on macOS,
/// `~/.cache/ms-playwright` on Linux, `%USERPROFILE%\AppData\Local\ms-playwright`
/// on Windows (playwright.dev, "Browsers", "Managing browser binaries").
pub(crate) const fn browsers_playwright(p: Platform) -> Rule {
    let paths = PerOs {
        macos: &["Library/Caches/ms-playwright"],
        linux: &[".cache/ms-playwright"],
        windows: &["AppData/Local/ms-playwright"],
    };
    Rule {
        hint: "reinstall: npx playwright install",
        ..rule("browsers.playwright", PackageCache, Review, Removal::Trash, Matcher::KnownPaths(paths.of(p)))
    }
}

pub(crate) const BROWSERS_PUPPETEER: Rule = Rule {
    hint: "reinstall: npx puppeteer browsers install",
    ..rule("browsers.puppeteer", PackageCache, Review, Removal::Trash, Matcher::KnownPaths(&[".cache/puppeteer"]))
};

/// Cypress's binary cache: `~/Library/Caches/Cypress` on macOS,
/// `~/.cache/Cypress` on Linux, `AppData/Local/Cypress/Cache` on Windows
/// (docs.cypress.io, "Advanced Installation", "Binary cache").
pub(crate) const fn browsers_cypress(p: Platform) -> Rule {
    let paths = PerOs {
        macos: &["Library/Caches/Cypress"],
        linux: &[".cache/Cypress"],
        windows: &["AppData/Local/Cypress/Cache"],
    };
    Rule {
        hint: "reinstall: npx cypress install",
        ..rule("browsers.cypress", PackageCache, Review, Removal::Trash, Matcher::KnownPaths(paths.of(p)))
    }
}

// The generic rule, lowest priority.

pub(crate) const GENERIC_CACHEDIR_TAG: Rule = Rule {
    hint: "tagged as a cache by its owner; regeneration may be slow",
    ..rule("generic.cachedir-tag", GenericCache, Review, Removal::Trash, Matcher::CacheDirTag)
};
