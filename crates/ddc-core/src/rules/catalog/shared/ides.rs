//! Shared rules: IDEs and the editors' remote servers.
//!
//! The owners name macOS app bundles. Elsewhere no process runs from a
//! bundle, so they match nothing there, and the open-file check of a
//! `Delete` stands in for them.

use super::super::{JETBRAINS, rule};
use super::PerOs;
use crate::class::Category::*;
use crate::class::Removal;
use crate::class::RiskClass::*;
use crate::platform::Platform;
use crate::rules::{Matcher, Rule, Scope};

/// The IDEs' caches folder: `~/Library/Caches/JetBrains` on macOS,
/// `~/.cache/JetBrains` on Linux, `%LOCALAPPDATA%\JetBrains` on Windows
/// (jetbrains.com, "Directories used by the IDE", the system directory).
/// Gateway's remote backends (`RemoteDev/dist`) sit inside it on Linux.
pub(crate) const fn ide_jetbrains_cache(p: Platform) -> Rule {
    let paths = PerOs {
        macos: &["Library/Caches/JetBrains"],
        linux: &[".cache/JetBrains"],
        windows: &["AppData/Local/JetBrains"],
    };
    Rule {
        owners: JETBRAINS,
        ..rule("ide.jetbrains-cache", Ide, Safe, Removal::Delete, Matcher::KnownPaths(paths.of(p)))
    }
}

/// Each IDE version's settings and plugins: `~/Library/Application
/// Support/JetBrains/<product><version>` on macOS; on Linux the settings in
/// `~/.config/JetBrains` and the plugins in `~/.local/share/JetBrains`;
/// `%APPDATA%\JetBrains` on Windows (the same page, the config and plugins
/// directories). The newest version of each product in each folder stays.
pub(crate) const fn ide_jetbrains_old_data(p: Platform) -> Rule {
    let paths = PerOs {
        macos: &["Library/Application Support/JetBrains"],
        linux: &[".config/JetBrains", ".local/share/JetBrains"],
        windows: &["AppData/Roaming/JetBrains"],
    };
    Rule {
        hint: "settings of an older IDE version; a newer one exists",
        scope: Scope::Siblings,
        ..rule("ide.jetbrains-old-data", Ide, Review, Removal::Trash, Matcher::KnownChildren(paths.of(p)))
    }
}

/// The caches in a VS Code-family editor's user data folder:
/// `~/Library/Application Support/<name>` on macOS, `~/.config/<name>` on
/// Linux, `%APPDATA%\<name>` on Windows (code.visualstudio.com, "Command
/// line", `--user-data-dir`). Cursor and Windsurf are forks with the same
/// layout under their own names.
macro_rules! editor_caches {
    ($root:literal, $name:literal) => {
        [
            concat!($root, "/", $name, "/Cache"),
            concat!($root, "/", $name, "/CachedData"),
            concat!($root, "/", $name, "/CachedExtensionVSIXs"),
            concat!($root, "/", $name, "/GPUCache"),
            concat!($root, "/", $name, "/logs"),
        ]
    };
}

macro_rules! editor_paths {
    ($name:literal) => {
        PerOs {
            macos: &editor_caches!("Library/Application Support", $name),
            linux: &editor_caches!(".config", $name),
            windows: &editor_caches!("AppData/Roaming", $name),
        }
    };
}

// One rule per editor, so a running VS Code does not hold back Cursor's caches.
pub(crate) const fn ide_vscode_cache(p: Platform) -> Rule {
    let paths = editor_paths!("Code");
    Rule {
        owners: &["Visual Studio Code"],
        ..rule("ide.vscode-cache", Ide, Safe, Removal::Delete, Matcher::KnownPaths(paths.of(p)))
    }
}

pub(crate) const fn ide_cursor_cache(p: Platform) -> Rule {
    let paths = editor_paths!("Cursor");
    Rule {
        owners: &["Cursor"],
        ..rule("ide.cursor-cache", Ide, Safe, Removal::Delete, Matcher::KnownPaths(paths.of(p)))
    }
}

pub(crate) const fn ide_windsurf_cache(p: Platform) -> Rule {
    let paths = editor_paths!("Windsurf");
    Rule {
        owners: &["Windsurf"],
        ..rule("ide.windsurf-cache", Ide, Safe, Removal::Delete, Matcher::KnownPaths(paths.of(p)))
    }
}

pub(crate) const fn ide_workspace_storage(p: Platform) -> Rule {
    let paths = PerOs {
        macos: &[
            "Library/Application Support/Code/User/workspaceStorage",
            "Library/Application Support/Cursor/User/workspaceStorage",
            "Library/Application Support/Windsurf/User/workspaceStorage",
        ],
        linux: &[
            ".config/Code/User/workspaceStorage",
            ".config/Cursor/User/workspaceStorage",
            ".config/Windsurf/User/workspaceStorage",
        ],
        windows: &[
            "AppData/Roaming/Code/User/workspaceStorage",
            "AppData/Roaming/Cursor/User/workspaceStorage",
            "AppData/Roaming/Windsurf/User/workspaceStorage",
        ],
    };
    Rule {
        hint: "chat history and workspace state",
        ..rule("ide.workspace-storage", Ide, UserData, Removal::None, Matcher::KnownPaths(paths.of(p)))
    }
}

/// What the editors' remote servers say: the next connection of that
/// editor version downloads its build again.
const SERVER_HINT: &str = "downloaded again when an editor of that version connects";

/// The servers VS Code installs on a machine it reaches over ssh, in the
/// same home-relative folders on every OS: the builds of the current layout
/// (`cli/servers/Stable-<commit>`, with the `.staging` folders of
/// interrupted installs beside them) and of the older one (`bin/<commit>`).
/// Which builds stay is decided from `lru.json` (`rules::toolchain`).
pub(crate) const IDE_VSCODE_SERVER: Rule = Rule {
    hint: SERVER_HINT,
    scope: Scope::Siblings,
    ..rule(
        "ide.vscode-server",
        Ide,
        Safe,
        Removal::Delete,
        Matcher::KnownChildren(&[
            ".vscode-server/cli/servers",
            ".vscode-server/bin",
            ".vscode-server-insiders/cli/servers",
            ".vscode-server-insiders/bin",
        ]),
    )
};

/// Cursor's servers: VS Code's layout under `~/.cursor-server`.
pub(crate) const IDE_CURSOR_SERVER: Rule = Rule {
    hint: SERVER_HINT,
    scope: Scope::Siblings,
    ..rule(
        "ide.cursor-server",
        Ide,
        Safe,
        Removal::Delete,
        Matcher::KnownChildren(&[".cursor-server/cli/servers", ".cursor-server/bin"]),
    )
};
