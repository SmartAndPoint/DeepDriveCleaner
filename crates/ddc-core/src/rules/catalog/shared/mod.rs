//! The rules every platform lists (story "Rule catalog split: shared and
//! per-OS sections", DDC-007). One id, one definition: a plain shared rule
//! is a `const`, the same relative path everywhere it can exist; a rule
//! whose tool keeps its folder in a different place on each OS is a
//! `const fn` of the platform, with its paths in a [`PerOs`] table and every
//! other field the same. [`all`] lists them in the macOS order; the
//! definitions sit by category in `projects`, `caches`, `tools` and `user`.
//!
//! A rule is shared with per-OS paths only where the tool's own
//! documentation gives its default folder on all three OSes; each `const fn`
//! cites it. The Windows paths are home-relative placeholders under
//! `AppData/Local` until story "Windows scanner and rule catalog" reads
//! `%LOCALAPPDATA%`. Every rule with `owners` is macOS-only: the owners are
//! macOS app bundle names.

mod caches;
mod projects;
mod tools;
mod user;

pub(crate) use self::{caches::*, projects::*, tools::*, user::*};
use crate::platform::Platform;
use crate::rules::Rule;

/// Folders in the home that every platform protects (safety ADR "Cleanup
/// safety model", §6, amended 2026-10-30): never a finding, never removed
/// themselves. `""` is the home itself.
pub(crate) const PROTECTED_IN_HOME: [&str; 12] = [
    "",
    "Documents",
    "Desktop",
    "Pictures",
    "Music",
    "Downloads",
    "Projects",
    ".config",
    ".ssh",
    ".gnupg",
    ".local",
    ".cache",
];

/// A rule's home-relative paths on each platform.
struct PerOs {
    macos: &'static [&'static str],
    linux: &'static [&'static str],
    windows: &'static [&'static str],
}

impl PerOs {
    const fn of(&self, p: Platform) -> &'static [&'static str] {
        match p {
            Platform::Macos => self.macos,
            Platform::Linux => self.linux,
            Platform::Windows => self.windows,
        }
    }
}

/// How many rules every platform shares.
pub(super) const COUNT: usize = 48;

/// Every shared rule resolved for `p`, in the macOS order.
pub(super) const fn all(p: Platform) -> [Rule; COUNT] {
    [
        USER_KEPT,
        DDC_TOMBSTONE,
        NODE_GLOBAL_PACKAGES,
        PYTHON_VENV,
        PYTHON_CONDA_ENV,
        PYTHON_TOX,
        PYTHON_PYCACHE,
        PYTHON_TOOL_CACHE,
        NODE_MODULES,
        NODE_MODULES_ORPHAN,
        JS_BUILD_CACHE,
        RUST_TARGET,
        MAVEN_TARGET,
        GRADLE_BUILD,
        GRADLE_PROJECT_CACHE,
        CMAKE_BUILD,
        COCOAPODS_PODS,
        cache_npm(p),
        cache_yarn(p),
        CACHE_BUN,
        cache_pnpm(p),
        cache_uv(p),
        CACHE_CONDA,
        cache_pip(p),
        cache_poetry(p),
        cache_go_build(p),
        CACHE_GO_MOD,
        CACHE_CARGO,
        CACHE_GRADLE,
        REPO_MAVEN,
        browsers_playwright(p),
        BROWSERS_PUPPETEER,
        browsers_cypress(p),
        VM_PODMAN,
        VM_COLIMA,
        VM_LIMA,
        TOOLCHAIN_NVM,
        TOOLCHAIN_PYENV,
        TOOLCHAIN_RUSTUP,
        MODEL_OLLAMA,
        MODEL_HUGGINGFACE,
        MODEL_LMSTUDIO,
        MODEL_TORCH,
        USER_PICTURES,
        user_movies(p),
        USER_MUSIC,
        USER_DOWNLOADS,
        GENERIC_CACHEDIR_TAG,
    ]
}

/// The ids of the shared rules, in the macOS order: an id on more than one
/// platform is one of these.
pub const SHARED_IDS: [&str; COUNT] = {
    let rules = all(Platform::Macos);
    let mut ids = [""; COUNT];
    let mut i = 0;
    while i < COUNT {
        ids[i] = rules[i].id;
        i += 1;
    }
    ids
};
