//! The macOS catalog: every rule, in match order (the first specialized
//! match wins, and a rule's index is its tag). The shared rules sit in it
//! where they match, resolved for macOS.

use super::rule;
use super::shared;
use crate::class::Category::*;
use crate::class::RiskClass::*;
use crate::class::{Removal, RiskClass};
use crate::model::{Origin, OriginHarness, OriginKind};
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
    // No owner by process name: §5's in-use check stays, and an owner match
    // would hold back every path whenever Claude Code runs.
    Rule {
        hint: "debug logs and legacy folders Claude Code no longer writes",
        ..agent(
            "agent.claude-code-scratch",
            Safe,
            Removal::Delete,
            Matcher::KnownPaths(&[".claude/debug", ".claude/todos", ".claude/statsig", ".claude/logs"]),
            OriginHarness::ClaudeCode,
            OriginKind::RuntimeCache,
        )
    },
    // Sessions are the entries of these two (story "Harness data rules"); the
    // folders themselves have no method, and each hint speaks of one session.
    Rule {
        hint: "Checkpoints of this session. /rewind cannot use them once moved.",
        ..agent(
            "agent.claude-code-file-history",
            Review,
            Removal::None,
            Matcher::KnownPaths(&[".claude/file-history"]),
            OriginHarness::ClaudeCode,
            OriginKind::SessionData,
        )
    },
    // One finding per project folder: no rule runs inside it, so its
    // `memory/` (auto memory) is never another rule's finding.
    Rule {
        hint: "This session's transcript. --resume of it fails until it is put back from the Trash, before it is resumed.",
        ..agent(
            "agent.claude-code-transcripts",
            Review,
            Removal::None,
            Matcher::KnownChildren(&[".claude/projects"]),
            OriginHarness::ClaudeCode,
            OriginKind::SessionData,
        )
    },
    Rule {
        hint: "A recalled prompt loses its pasted text; past /insights reports are gone.",
        ..agent(
            "agent.claude-code-history",
            Review,
            Removal::Trash,
            Matcher::KnownPaths(&[".claude/paste-cache", ".claude/usage-data"]),
            OriginHarness::ClaudeCode,
            OriginKind::SessionData,
        )
    },
    // Codex, opencode and Cursor go to the Trash only while their harness is
    // not live: the guard refuses a running one, or a held Codex lock, as
    // `agent-live` (story "Harness data rules").
    Rule {
        hint: "Codex's past threads; Codex cannot reopen them once moved. Quit Codex, and its app-server, to move them.",
        ..agent(
            "agent.codex-sessions",
            Review,
            Removal::Trash,
            Matcher::KnownPaths(&[".codex/sessions", ".codex/log"]),
            OriginHarness::Codex,
            OriginKind::SessionData,
        )
    },
    Rule {
        hint: "Downloaded runtimes; Codex fetches them again on demand. Quit Codex to move them.",
        ..agent(
            "agent.codex-runtimes",
            Review,
            Removal::Trash,
            Matcher::KnownPaths(&[".cache/codex-runtimes"]),
            OriginHarness::Codex,
            OriginKind::RuntimeCache,
        )
    },
    // By subfolder, never whole: `.local/share/opencode` also holds
    // `auth.json` (provider credentials) and `opencode.db`, its live index.
    Rule {
        hint: "opencode's stored sessions, snapshots and logs; opencode cannot show those sessions once moved. Quit opencode to move them.",
        ..agent(
            "agent.opencode-data",
            Review,
            Removal::Trash,
            Matcher::KnownPaths(&[
                ".local/share/opencode/storage",
                ".local/share/opencode/snapshot",
                ".local/share/opencode/log",
                ".cache/opencode",
            ]),
            OriginHarness::Opencode,
            OriginKind::SessionData,
        )
    },
    Rule {
        hint: "Cursor's agent transcripts per project, and its AI-code tracking; Cursor cannot show them once moved. Quit Cursor to move them.",
        ..agent(
            "agent.cursor-agent-data",
            Review,
            Removal::Trash,
            Matcher::KnownPaths(&[".cursor/projects", ".cursor/ai-tracking"]),
            OriginHarness::Cursor,
            OriginKind::SessionData,
        )
    },
    // Sealed, like app bundles: never a finding, and no rule fires inside.
    // The live session records; the shell snapshots a live session sources
    // before every Bash command without holding them open; what the person
    // installed; the person's plans and chats. `.cursor/worktrees` and
    // `.codex/worktrees` are not sealed: the rules work inside a worktree.
    Rule {
        hint: "Claude Code's live session records, shell snapshots and installed plugins",
        listed: false,
        ..rule(
            "agent.claude-code-sealed",
            Personal,
            UserData,
            Removal::None,
            Matcher::KnownPaths(&[".claude/sessions", ".claude/shell-snapshots", ".claude/plugins"]),
        )
    },
    Rule {
        hint: "Cursor's installed extensions and plugins, and the person's plans and chats",
        listed: false,
        ..rule(
            "agent.cursor-sealed",
            Personal,
            UserData,
            Removal::None,
            Matcher::KnownPaths(&[".cursor/extensions", ".cursor/plugins", ".cursor/plans", ".cursor/chats"]),
        )
    },
    // ---- application caches -------------------------------------------------------
    Rule {
        owners: &["Google Chrome"],
        ..rule(
            "app.chrome-cache",
            AppCache,
            Safe,
            Removal::Delete,
            Matcher::KnownPaths(&["Library/Caches/Google/Chrome"]),
        )
    },
    Rule {
        owners: &["Brave Browser"],
        ..rule(
            "app.brave-cache",
            AppCache,
            Safe,
            Removal::Delete,
            Matcher::KnownPaths(&["Library/Caches/BraveSoftware"]),
        )
    },
    Rule {
        owners: &["Firefox"],
        ..rule("app.firefox-cache", AppCache, Safe, Removal::Delete, Matcher::KnownPaths(&["Library/Caches/Firefox"]))
    },
    Rule {
        owners: &["Spotify"],
        ..rule(
            "app.spotify-cache",
            AppCache,
            Safe,
            Removal::Delete,
            Matcher::KnownPaths(&["Library/Caches/com.spotify.client"]),
        )
    },
    Rule {
        owners: &["Slack"],
        ..rule(
            "app.slack-cache",
            AppCache,
            Safe,
            Removal::Delete,
            Matcher::KnownPaths(&[
                "Library/Application Support/Slack/Cache",
                "Library/Application Support/Slack/Service Worker/CacheStorage",
            ]),
        )
    },
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
    Rule {
        hint: "empty the Trash in Finder",
        ..rule("trash", Trash, Review, Removal::None, Matcher::KnownPaths(&[".Trash"]))
    },
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

/// A rule of a coding agent's harness. Its `review` rules are `listed_only`:
/// only a person removes them, never `--yes` (ADR "Agent footprint:
/// worktrees, harness data and bounded consent for agents", §3).
const fn agent(
    id: &'static str,
    class: RiskClass,
    removal: Removal,
    matcher: Matcher,
    harness: OriginHarness,
    kind: OriginKind,
) -> Rule {
    Rule {
        origin: Some(Origin { harness, kind }),
        listed_only: matches!(class, RiskClass::Review),
        ..rule(id, AppCache, class, removal, matcher)
    }
}
