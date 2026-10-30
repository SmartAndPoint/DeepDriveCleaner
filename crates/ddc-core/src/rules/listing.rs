//! `ddc rules --json`: every rule with its group, the source of the app's
//! Types settings (ADR "Menu bar app: Swift shell over the ddc CLI", §6 item 8).
//! Schema: `docs/rules.schema.json`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{KEPT, Rule, TOMBSTONE, rules_of};
use crate::class::{Category, RiskClass};
use crate::model::RemovalKind;
use crate::plan::action_for;
use crate::platform::{PLATFORM, Platform};

/// Version of [`RuleList`]; the scan report has its own.
pub const RULES_SCHEMA_VERSION: u32 = 1;

/// Every rule id in exactly one group, in the order of the Types settings:
/// the thirteen of the design canvas's revised Settings (artboard 10) and
/// "Maven and test browsers", which holds the `review` downloads out of the
/// Safe "Toolchain caches", then three without a switch: ddc's own
/// leftovers, the Trash (story "The Trash: its own row, not Your data") and
/// the user's data.
pub const GROUPS: &[(&str, &[&str])] = &[
    ("Python environments", &["python.venv", "python.conda-env", "python.tox"]),
    ("node_modules", &["node.node_modules", "node.node_modules-orphan"]),
    ("Build outputs", &["rust.target", "maven.target", "gradle.build", "cmake.build", "cocoapods.pods"]),
    ("Build caches in projects", &["js.build-cache", "python.pycache", "python.tool-cache", "gradle.project-cache"]),
    (
        "Package downloads",
        &[
            "cache.npm",
            "cache.pnpm",
            "cache.yarn",
            "cache.bun",
            "cache.pip",
            "cache.conda",
            "cache.poetry",
            "cache.homebrew",
            "cache.cocoapods",
        ],
    ),
    ("Toolchain caches", &["cache.cargo", "cache.uv", "cache.go-mod", "cache.go-build", "cache.gradle"]),
    ("Maven and test browsers", &["repo.maven", "browsers.playwright", "browsers.puppeteer", "browsers.cypress"]),
    (
        "Browser and app caches",
        &[
            "app.chrome-cache",
            "app.brave-cache",
            "app.firefox-cache",
            "app.spotify-cache",
            "app.slack-cache",
            "app.adobe-camera-raw-cache",
            "generic.cachedir-tag",
        ],
    ),
    (
        "IDE caches",
        &[
            "ide.jetbrains-cache",
            "ide.jetbrains-old-data",
            "ide.vscode-cache",
            "ide.cursor-cache",
            "ide.windsurf-cache",
        ],
    ),
    ("Xcode builds", &["xcode.derived-data", "xcode.simulator-caches"]),
    ("Xcode device support", &["xcode.device-support"]),
    ("Old toolchains", &["toolchain.nvm", "toolchain.pyenv", "toolchain.rustup"]),
    ("ML models", &["model.ollama", "model.huggingface", "model.lmstudio", "model.torch"]),
    (
        "VM and container images",
        &[
            "vm.docker-desktop",
            "vm.podman",
            "vm.colima",
            "vm.lima",
            "vm.orbstack",
            "vm.rancher-desktop",
            "cache.colima",
        ],
    ),
    ("Interrupted cleanups", &[TOMBSTONE]),
    ("Trash", &["trash"]),
    (
        "Your data",
        &[
            KEPT,
            "user.pictures",
            "user.movies",
            "user.music",
            "user.downloads",
            "user.ios-backups",
            "user.imazing-backups",
            "user.telegram",
            "user.whatsapp",
            "user.cloud",
            "xcode.archives",
            "ide.workspace-storage",
            "macos.app-bundle",
            "node.global-packages",
        ],
    ),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RuleList {
    pub schema_version: u32,
    /// Group labels in display order.
    pub groups: Vec<String>,
    pub rules: Vec<RuleInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RuleInfo {
    pub id: String,
    pub group: String,
    pub category: Category,
    pub class: RiskClass,
    pub removal: RemovalKind,
    /// Shown in reports; unlisted rules only stop other rules from matching inside.
    pub listed: bool,
    /// The Types settings may switch it off (`disabled_rules`). False for
    /// ddc's own leftovers and for rules ddc never acts on.
    pub switchable: bool,
}

/// The group of a rule id; `None` for an id in no group.
pub fn group_of(id: &str) -> Option<&'static str> {
    GROUPS.iter().find(|(_, ids)| ids.contains(&id)).map(|(label, _)| *label)
}

pub fn switchable(rule: &Rule) -> bool {
    rule.id != TOMBSTONE && action_for(rule, rule.class, false).is_some()
}

impl RuleList {
    /// The rules of the platform this build is for.
    pub fn builtin() -> Self {
        Self::for_platform(PLATFORM)
    }

    /// The rules of platform `p`, with the groups that hold one of them.
    pub fn for_platform(p: Platform) -> Self {
        let rules = rules_of(p);
        Self {
            schema_version: RULES_SCHEMA_VERSION,
            groups: GROUPS
                .iter()
                .filter(|(_, ids)| rules.iter().any(|r| ids.contains(&r.id)))
                .map(|(label, _)| (*label).to_owned())
                .collect(),
            rules: rules
                .iter()
                .map(|r| RuleInfo {
                    id: r.id.to_owned(),
                    group: group_of(r.id).unwrap_or_default().to_owned(),
                    category: r.category,
                    class: r.class,
                    removal: (&r.removal).into(),
                    listed: r.listed,
                    switchable: switchable(r),
                })
                .collect(),
        }
    }
}
