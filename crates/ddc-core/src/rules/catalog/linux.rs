//! The Linux catalog: the shared rules, in the macOS order, resolved for
//! Linux. Story "Linux scanner and rule catalog" adds the rules only Linux has.

use super::shared;
use crate::platform::Platform;
use crate::rules::Rule;

/// How many rules Linux has.
const COUNT: usize = 48;

/// A named `static`, so the list has one address (its rules' tags are
/// found by it).
pub(super) static RULES: [Rule; COUNT] = [
    shared::USER_KEPT,
    shared::DDC_TOMBSTONE,
    shared::NODE_GLOBAL_PACKAGES,
    shared::PYTHON_VENV,
    shared::PYTHON_CONDA_ENV,
    shared::PYTHON_TOX,
    shared::PYTHON_PYCACHE,
    shared::PYTHON_TOOL_CACHE,
    shared::NODE_MODULES,
    shared::NODE_MODULES_ORPHAN,
    shared::JS_BUILD_CACHE,
    shared::RUST_TARGET,
    shared::MAVEN_TARGET,
    shared::GRADLE_BUILD,
    shared::GRADLE_PROJECT_CACHE,
    shared::CMAKE_BUILD,
    shared::COCOAPODS_PODS,
    shared::cache_npm(Platform::Linux),
    shared::cache_yarn(Platform::Linux),
    shared::CACHE_BUN,
    shared::cache_pnpm(Platform::Linux),
    shared::cache_uv(Platform::Linux),
    shared::CACHE_CONDA,
    shared::cache_pip(Platform::Linux),
    shared::cache_poetry(Platform::Linux),
    shared::cache_go_build(Platform::Linux),
    shared::CACHE_GO_MOD,
    shared::CACHE_CARGO,
    shared::CACHE_GRADLE,
    shared::REPO_MAVEN,
    shared::browsers_playwright(Platform::Linux),
    shared::BROWSERS_PUPPETEER,
    shared::browsers_cypress(Platform::Linux),
    shared::VM_PODMAN,
    shared::VM_COLIMA,
    shared::VM_LIMA,
    shared::TOOLCHAIN_NVM,
    shared::TOOLCHAIN_PYENV,
    shared::TOOLCHAIN_RUSTUP,
    shared::MODEL_OLLAMA,
    shared::MODEL_HUGGINGFACE,
    shared::MODEL_LMSTUDIO,
    shared::MODEL_TORCH,
    shared::USER_PICTURES,
    shared::user_movies(Platform::Linux),
    shared::USER_MUSIC,
    shared::USER_DOWNLOADS,
    shared::GENERIC_CACHEDIR_TAG,
];
