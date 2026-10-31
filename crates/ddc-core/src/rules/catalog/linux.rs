//! The Linux catalog: the shared rules, in the macOS order, resolved for
//! Linux, and the rules only Linux has (story "Linux scanner and rule
//! catalog", DDC-007).

use super::{rule, shared};
use crate::class::Category::*;
use crate::class::Removal;
use crate::class::RiskClass::*;
use crate::platform::Platform;
use crate::rules::{Matcher, Rule};

/// The folders in the home only Linux protects (safety ADR "Cleanup safety
/// model", §6, amendment of 2026-10-31): XDG's data, state and program
/// folders, the keyrings, the Trash, the NSS certificate store, browser
/// and mail profiles, and `~/Videos`. The folders themselves, not their
/// contents: a cache rule still matches inside `~/.local/share`.
pub(super) const OWN_PROTECTED_IN_HOME: [&str; 9] = [
    ".local/share",
    ".local/state",
    ".local/bin",
    ".local/share/keyrings",
    ".local/share/Trash",
    ".pki",
    ".mozilla",
    ".thunderbird",
    "Videos",
];

/// The shared protected folders and Linux's own.
pub(super) static PROTECTED_IN_HOME: [&str; 21] = super::concat(&shared::PROTECTED_IN_HOME, &OWN_PROTECTED_IN_HOME);

/// Neither removed nor walked (§6, amendment of 2026-10-31): root Docker's
/// and root Podman's storage.
pub(super) static NEVER_WALKED: [&str; 2] = ["/var/lib/docker", "/var/lib/containers"];

/// System folders that are never removed (§6, amendment of 2026-10-31).
pub(super) static PROTECTED_SYSTEM: [&str; 7] = ["/", "/home", "/usr", "/etc", "/opt", "/var", "/boot"];

const COUNT: usize = shared::COUNT + 1;

/// The rules, in match order: a named `static`, so the list has one address
/// (its rules' tags are found by it).
pub(super) static RULES: [Rule; COUNT] = [
    // ---- synthetic -------------------------------------------------------
    shared::USER_KEPT,
    // ---- project artifacts (specialized, in match order) -----------------
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
    // ---- package caches ---------------------------------------------------
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
    // ---- VMs and containers: native-only ----------------------------------
    shared::vm_docker_desktop(Platform::Linux),
    // Rootless Docker keeps its images and volumes in the home (docs.docker.com,
    // "Rootless mode", the data dir). Its daemon serves them; `ddc` only reports.
    Rule {
        hint: "manage with docker: docker image prune, docker builder prune",
        ..rule(
            "vm.docker-rootless",
            VmContainer,
            Review,
            Removal::NativeOnly(&[]),
            Matcher::KnownPaths(&[".local/share/docker"]),
        )
    },
    shared::VM_PODMAN,
    shared::VM_COLIMA,
    shared::VM_LIMA,
    // ---- toolchains (each version separately; protected versions dropped after the walk)
    shared::TOOLCHAIN_NVM,
    shared::TOOLCHAIN_PYENV,
    shared::TOOLCHAIN_RUSTUP,
    // ---- ML models -----------------------------------------------------------
    shared::MODEL_OLLAMA,
    shared::MODEL_HUGGINGFACE,
    shared::MODEL_LMSTUDIO,
    shared::MODEL_TORCH,
    // ---- IDEs ------------------------------------------------------------------
    shared::ide_jetbrains_cache(Platform::Linux),
    shared::ide_jetbrains_old_data(Platform::Linux),
    shared::ide_vscode_cache(Platform::Linux),
    shared::ide_cursor_cache(Platform::Linux),
    shared::ide_windsurf_cache(Platform::Linux),
    shared::ide_workspace_storage(Platform::Linux),
    shared::IDE_VSCODE_SERVER,
    shared::IDE_CURSOR_SERVER,
    // ---- coding agents (ADR "Agent footprint: worktrees, harness data and
    // bounded consent for agents", §3) ---------------------------------------------
    shared::AGENT_CLAUDE_CODE_SCRATCH,
    shared::AGENT_CLAUDE_CODE_FILE_HISTORY,
    shared::AGENT_CLAUDE_CODE_TRANSCRIPTS,
    shared::AGENT_CLAUDE_CODE_HISTORY,
    shared::AGENT_CODEX_SESSIONS,
    shared::AGENT_CODEX_RUNTIMES,
    shared::AGENT_OPENCODE_DATA,
    shared::AGENT_CURSOR_AGENT_DATA,
    shared::AGENT_CLAUDE_CODE_SEALED,
    shared::AGENT_CURSOR_SEALED,
    // ---- application caches -------------------------------------------------------
    shared::app_chrome_cache(Platform::Linux),
    shared::app_brave_cache(Platform::Linux),
    shared::app_firefox_cache(Platform::Linux),
    shared::app_spotify_cache(Platform::Linux),
    shared::app_slack_cache(Platform::Linux),
    shared::trash(Platform::Linux),
    // ---- user data (report only) --------------------------------------------------
    shared::USER_PICTURES,
    shared::user_movies(Platform::Linux),
    shared::USER_MUSIC,
    shared::USER_DOWNLOADS,
    // ---- generic, lowest priority -------------------------------------------------------
    shared::GENERIC_CACHEDIR_TAG,
];
