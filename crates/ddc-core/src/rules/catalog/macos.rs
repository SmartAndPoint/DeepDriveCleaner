//! The macOS catalog: every rule, in match order (the first specialized
//! match wins, and a rule's index is its tag). The shared rules sit in it
//! where they match, resolved for macOS.

use super::rule;
use super::shared;
use crate::class::Category::*;
use crate::class::Removal;
use crate::class::RiskClass::*;
use crate::platform::Platform;
use crate::rules::{Matcher, Rule};

/// The folders in the home only macOS protects: `~/Movies`, `~/Library`
/// and its protected subfolders, the cloud folders among them (safety ADR
/// "Cleanup safety model", §6).
pub(super) const OWN_PROTECTED_IN_HOME: [&str; 8] = [
    "Movies",
    "Library",
    "Library/Caches",
    "Library/Application Support",
    "Library/Containers",
    "Library/Group Containers",
    "Library/Mobile Documents",
    "Library/CloudStorage",
];

/// The shared protected folders and macOS's own: §6's list in the home.
pub(super) static PROTECTED_IN_HOME: [&str; 20] = super::concat(&shared::PROTECTED_IN_HOME, &OWN_PROTECTED_IN_HOME);

/// System folders that are never removed (§6).
pub(super) static PROTECTED_SYSTEM: [&str; 8] =
    ["/", "/System", "/Applications", "/Library", "/usr", "/opt", "/opt/homebrew", "/Users"];

/// How many rules macOS has.
const COUNT: usize = 89;

/// A named `static`, so the list has one address (its rules' tags are
/// found by it).
pub(super) static RULES: [Rule; COUNT] = [
    // ---- synthetic -------------------------------------------------------
    shared::USER_KEPT,
    // ---- project artifacts (specialized, in match order) -----------------
    shared::DDC_TOMBSTONE,
    // App bundles are sealed: rules never apply inside (`.app/Contents/Resources/node_modules`).
    Rule {
        hint: "macOS application bundle",
        listed: false,
        ..rule("macos.app-bundle", Personal, UserData, Removal::None, Matcher::NameSuffix(".app"))
    },
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
    shared::cache_npm(Platform::Macos),
    shared::cache_yarn(Platform::Macos),
    shared::CACHE_BUN,
    shared::cache_pnpm(Platform::Macos),
    shared::cache_uv(Platform::Macos),
    shared::CACHE_CONDA,
    shared::cache_pip(Platform::Macos),
    shared::cache_poetry(Platform::Macos),
    rule("cache.homebrew", PackageCache, Safe, Removal::Delete, Matcher::KnownPaths(&["Library/Caches/Homebrew"])),
    shared::cache_go_build(Platform::Macos),
    shared::CACHE_GO_MOD,
    shared::CACHE_CARGO,
    shared::CACHE_GRADLE,
    shared::REPO_MAVEN,
    rule("cache.cocoapods", PackageCache, Safe, Removal::Delete, Matcher::KnownPaths(&["Library/Caches/CocoaPods"])),
    shared::browsers_playwright(Platform::Macos),
    shared::BROWSERS_PUPPETEER,
    shared::browsers_cypress(Platform::Macos),
    // ---- Xcode ------------------------------------------------------------
    Rule {
        owners: &["Xcode"],
        ..rule(
            "xcode.derived-data",
            Xcode,
            Safe,
            Removal::Delete,
            Matcher::KnownPaths(&["Library/Developer/Xcode/DerivedData"]),
        )
    },
    rule(
        "xcode.simulator-caches",
        Xcode,
        Safe,
        Removal::Delete,
        Matcher::KnownPaths(&["Library/Developer/CoreSimulator/Caches"]),
    ),
    Rule {
        hint: "re-created when the device is connected again",
        ..rule(
            "xcode.device-support",
            Xcode,
            Review,
            Removal::Trash,
            Matcher::KnownPaths(&["Library/Developer/Xcode/iOS DeviceSupport"]),
        )
    },
    Rule {
        hint: "archives contain dSYMs needed to symbolicate crash reports",
        ..rule(
            "xcode.archives",
            Xcode,
            UserData,
            Removal::None,
            Matcher::KnownPaths(&["Library/Developer/Xcode/Archives"]),
        )
    },
    // ---- VMs and containers: native-only ----------------------------------
    shared::vm_docker_desktop(Platform::Macos),
    shared::VM_PODMAN,
    shared::VM_COLIMA,
    shared::VM_LIMA,
    Rule {
        hint: "manage in the OrbStack app",
        ..rule("vm.orbstack", VmContainer, Review, Removal::NativeOnly(&[]), Matcher::KnownPaths(&[".orbstack"]))
    },
    Rule {
        hint: "manage in Rancher Desktop (Troubleshooting → Factory Reset)",
        ..rule(
            "vm.rancher-desktop",
            VmContainer,
            Review,
            Removal::NativeOnly(&[]),
            Matcher::KnownPaths(&["Library/Application Support/rancher-desktop"]),
        )
    },
    rule("cache.colima", PackageCache, Safe, Removal::Delete, Matcher::KnownPaths(&["Library/Caches/colima"])),
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
    shared::ide_jetbrains_cache(Platform::Macos),
    shared::ide_jetbrains_old_data(Platform::Macos),
    shared::ide_vscode_cache(Platform::Macos),
    shared::ide_cursor_cache(Platform::Macos),
    shared::ide_windsurf_cache(Platform::Macos),
    shared::ide_workspace_storage(Platform::Macos),
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
    shared::app_chrome_cache(Platform::Macos),
    shared::app_brave_cache(Platform::Macos),
    shared::app_firefox_cache(Platform::Macos),
    shared::app_spotify_cache(Platform::Macos),
    shared::app_slack_cache(Platform::Macos),
    Rule {
        owners: &["Adobe Lightroom", "Adobe Photoshop"],
        ..rule(
            "app.adobe-camera-raw-cache",
            AppCache,
            Safe,
            Removal::Delete,
            Matcher::KnownPaths(&["Library/Caches/Adobe Camera Raw 2"]),
        )
    },
    shared::trash(Platform::Macos),
    // ---- user data (report only) --------------------------------------------------
    shared::USER_PICTURES,
    shared::user_movies(Platform::Macos),
    shared::USER_MUSIC,
    shared::USER_DOWNLOADS,
    rule(
        "user.ios-backups",
        Personal,
        UserData,
        Removal::None,
        Matcher::KnownPaths(&["Library/Application Support/MobileSync/Backup"]),
    ),
    rule(
        "user.imazing-backups",
        Personal,
        UserData,
        Removal::None,
        Matcher::KnownPaths(&["Library/Application Support/iMazing/Backups"]),
    ),
    rule(
        "user.telegram",
        Personal,
        UserData,
        Removal::None,
        Matcher::KnownPaths(&["Library/Group Containers/6N38VWS5BX.ru.keepcoder.Telegram"]),
    ),
    rule(
        "user.whatsapp",
        Personal,
        UserData,
        Removal::None,
        Matcher::KnownPaths(&["Library/Group Containers/group.net.whatsapp.WhatsApp.shared"]),
    ),
    Rule {
        hint: "cloud folders are not scanned",
        ..rule(
            "user.cloud",
            Personal,
            UserData,
            Removal::None,
            Matcher::NoTraverse(&["Library/CloudStorage", "Library/Mobile Documents"]),
        )
    },
    // ---- generic, lowest priority -------------------------------------------------------
    shared::GENERIC_CACHEDIR_TAG,
];
