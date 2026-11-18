//! The Windows catalog: the shared rules, in the macOS order, resolved for
//! Windows, and the rules only Windows has (story "Windows scanner and rule
//! catalog", DDC-007, step 13; safety ADR §6, Windows, neither removed nor
//! walked, planned 2026-11-17).

use super::{rule, shared};
use crate::class::Category::*;
use crate::class::Removal;
use crate::class::RiskClass::*;
use crate::platform::Platform;
use crate::platform::types::Redirect;
use crate::rules::{Matcher, Rule};

/// The folders in the home only Windows protects (safety ADR §6, Windows,
/// in the home): `AppData` and its three children, the credentials, keys,
/// certificates and cookies Windows keeps under them, the browsers' and
/// Thunderbird's profiles, gpg's folder, the store apps' and WSL's data,
/// Visual Studio's projects, and the user folders the shared section does
/// not name. Spelled with `/`, as the rules' paths are; their ancestors are
/// protected through `rules::Protected`, so `AppData/Roaming/Microsoft` is
/// not listed.
pub(super) const OWN_PROTECTED_IN_HOME: [&str; 28] = [
    "AppData",
    "AppData/Roaming",
    "AppData/Local",
    "AppData/LocalLow",
    "AppData/Roaming/Microsoft/Protect",
    "AppData/Roaming/Microsoft/Crypto",
    "AppData/Roaming/Microsoft/SystemCertificates",
    "AppData/Roaming/Microsoft/Credentials",
    "AppData/Local/Microsoft/Credentials",
    "AppData/Local/Microsoft/Vault",
    "AppData/Local/Microsoft/Windows/INetCookies",
    "AppData/Local/Google/Chrome/User Data",
    "AppData/Local/Microsoft/Edge/User Data",
    "AppData/Local/BraveSoftware/Brave-Browser/User Data",
    "AppData/Roaming/Mozilla/Firefox/Profiles",
    "AppData/Roaming/Thunderbird/Profiles",
    "AppData/Roaming/gnupg",
    "AppData/Local/Packages",
    "AppData/Local/wsl",
    "source",
    "source/repos",
    "OneDrive",
    "Dropbox",
    "Box",
    "iCloudDrive",
    "Videos",
    "Favorites",
    "Saved Games",
];

/// The shared protected folders and Windows's own.
pub(super) static PROTECTED_IN_HOME: [&str; 40] = super::concat(&shared::PROTECTED_IN_HOME, &OWN_PROTECTED_IN_HOME);

/// System folders that are never removed (safety ADR §6, Windows, system
/// folders), at their default places. The guard adds where the known-folder
/// API says they are and their drive roots (`platform::folders::system_folders`),
/// and refuses every drive root by its shape.
pub(super) static PROTECTED_SYSTEM: [&str; 6] =
    [r"C:\", r"C:\Windows", r"C:\Program Files", r"C:\Program Files (x86)", r"C:\ProgramData", r"C:\Users"];

/// The default places of the folders a variable redirects: an entry below
/// one is protected below the variable's value too (safety ADR §6, Windows,
/// system folders, planned 2026-11-08).
pub(super) static REDIRECTABLE: [(Redirect, &str); 2] =
    [(Redirect::Roaming, "AppData/Roaming"), (Redirect::Local, "AppData/Local")];

/// How to manage a WSL distribution's disk, which ddc only reports.
const WSL_HINT: &str =
    "manage with wsl, see learn.microsoft.com/en-us/windows/wsl/disk-space; ddc never touches the disk";

const COUNT: usize = shared::COUNT + 5;

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
    // ---- linked git worktrees: regions, matched before the `.git` refusal ----
    shared::GIT_WORKTREE,
    shared::GIT_WORKTREE_ORPHAN,
    // ---- package caches ---------------------------------------------------
    shared::cache_npm(Platform::Windows),
    shared::cache_yarn(Platform::Windows),
    shared::CACHE_BUN,
    shared::cache_pnpm(Platform::Windows),
    shared::cache_uv(Platform::Windows),
    shared::CACHE_CONDA,
    shared::cache_pip(Platform::Windows),
    shared::cache_poetry(Platform::Windows),
    shared::cache_go_build(Platform::Windows),
    shared::CACHE_GO_MOD,
    shared::CACHE_CARGO,
    shared::CACHE_GRADLE,
    shared::REPO_MAVEN,
    // NuGet's HTTP cache, and its global packages folder: builds reference
    // the second directly and it can hold packages built locally, as Maven's
    // repository can (learn.microsoft.com, NuGet's page on the global
    // packages, cache and temp folders).
    rule("cache.nuget", PackageCache, Safe, Removal::Delete, Matcher::KnownPaths(&["AppData/Local/NuGet/v3-cache"])),
    Rule {
        hint: "builds use it directly; may hold packages built locally",
        ..rule("repo.nuget", PackageCache, Review, Removal::Trash, Matcher::KnownPaths(&[".nuget/packages"]))
    },
    shared::browsers_playwright(Platform::Windows),
    shared::BROWSERS_PUPPETEER,
    shared::browsers_cypress(Platform::Windows),
    // ---- VMs and containers: native-only ----------------------------------
    shared::vm_docker_desktop(Platform::Windows),
    // WSL's distributions: a VM's disk, reported only; ddc never touches a
    // `.vhdx`. A folder of `AppData\Local\wsl` holding one, whatever its name,
    // and a store package's `LocalState\ext4.vhdx`.
    Rule {
        hint: WSL_HINT,
        ..rule(
            "vm.wsl",
            VmContainer,
            Review,
            Removal::NativeOnly(&[]),
            Matcher::ChildHolding { parents: &["AppData/Local/wsl"], file: "ext4.vhdx" },
        )
    },
    Rule {
        hint: WSL_HINT,
        ..rule(
            "vm.wsl-store",
            VmContainer,
            Review,
            Removal::NativeOnly(&[]),
            Matcher::ChildHolding { parents: &["AppData/Local/Packages"], file: "LocalState/ext4.vhdx" },
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
    shared::ide_jetbrains_cache(Platform::Windows),
    shared::ide_jetbrains_old_data(Platform::Windows),
    shared::ide_vscode_cache(Platform::Windows),
    shared::ide_cursor_cache(Platform::Windows),
    shared::ide_windsurf_cache(Platform::Windows),
    shared::ide_workspace_storage(Platform::Windows),
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
    shared::agent_claude_code_builds(Platform::Windows),
    shared::agent_cursor_agent_builds(Platform::Windows),
    shared::AGENT_CLAUDE_CODE_SEALED,
    shared::AGENT_SHARED_SKILLS_SEALED,
    shared::AGENT_CURSOR_SEALED,
    // ---- application caches -------------------------------------------------------
    shared::app_chrome_cache(Platform::Windows),
    shared::app_brave_cache(Platform::Windows),
    shared::app_firefox_cache(Platform::Windows),
    shared::app_spotify_cache(Platform::Windows),
    shared::app_slack_cache(Platform::Windows),
    shared::trash(Platform::Windows),
    // The children of the temporary folder, never the folder itself: an
    // installer keeps its files there between its steps, and a file a
    // running program holds is refused by the in-use checks.
    Rule {
        hint: "a program or an installer may still need it",
        ..rule("windows.temp", AppCache, Review, Removal::Trash, Matcher::KnownChildren(&["AppData/Local/Temp"]))
    },
    // ---- user data (report only) --------------------------------------------------
    shared::USER_PICTURES,
    shared::user_movies(Platform::Windows),
    shared::USER_MUSIC,
    shared::USER_DOWNLOADS,
    shared::user_cloud(Platform::Windows),
    // ---- generic, lowest priority -------------------------------------------------------
    shared::GENERIC_CACHEDIR_TAG,
];
