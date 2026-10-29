//! Risk classes, categories and removal methods — ADR "Cleanup safety model:
//! risk classes, plan and confirmation", §1.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RiskClass {
    /// Cache that refills itself.
    Safe,
    /// Project artifact restored by install (from a lock file) or by a build.
    Rebuildable,
    /// Expensive to restore, or restorability is uncertain.
    Review,
    /// The user's data: reported, never planned.
    UserData,
}

impl RiskClass {
    pub const ALL: [RiskClass; 4] = [RiskClass::Safe, RiskClass::Rebuildable, RiskClass::Review, RiskClass::UserData];

    pub fn as_str(self) -> &'static str {
        match self {
            RiskClass::Safe => "safe",
            RiskClass::Rebuildable => "rebuildable",
            RiskClass::Review => "review",
            RiskClass::UserData => "user-data",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    ProjectArtifact,
    PackageCache,
    Xcode,
    VmContainer,
    Toolchain,
    MlModel,
    Ide,
    AppCache,
    Trash,
    GenericCache,
    /// Photos, backups, downloads, messengers.
    Personal,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::ProjectArtifact => "project-artifact",
            Category::PackageCache => "package-cache",
            Category::Xcode => "xcode",
            Category::VmContainer => "vm-container",
            Category::Toolchain => "toolchain",
            Category::MlModel => "ml-model",
            Category::Ide => "ide",
            Category::AppCache => "app-cache",
            Category::Trash => "trash",
            Category::GenericCache => "generic-cache",
            Category::Personal => "personal",
        }
    }
}

/// How a native command is checked to act on exactly the finding, and only
/// where the tool reports the finding as its own (ADR §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// `program`, found on the tool path, prints its location.
    Probe(LocationProbe),
    /// The known conda installs, each asked for its folders (safety ADR,
    /// amendment of 2026-10-29). The command runs as the claiming install's
    /// `bin/conda`, never as a `conda` found on the tool path.
    Conda(CondaFolder),
}

/// What a conda finding is to conda.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CondaFolder {
    /// A child of one of an install's `envs_dirs`, holding `conda-meta/history`.
    Environment,
    /// One of an install's `pkgs_dirs`.
    PackageCache,
}

/// How a tool reports its own location, to check that a native command acts
/// on exactly the finding (ADR §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocationProbe {
    pub program: &'static str,
    pub args: &'static [&'static str],
    pub compare: LocationCompare,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocationCompare {
    /// The reported path equals the finding.
    Equals,
    /// The reported path is the root the finding lives in.
    Contains,
}

/// How a runtime shows it answers, before its prune commands run (ADR §1,
/// `native-only`: "only when the daemon is running"). The executor runs the
/// probe; the dry run runs nothing and reads `process` and `context_meta`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadyProbe {
    pub program: &'static str,
    pub args: &'static [&'static str],
    /// The runtime's own process, by executable name.
    pub process: &'static str,
    /// The context's metadata file, relative to the docker config folder.
    pub context_meta: &'static str,
    /// Why it is not ready when it does not answer: a timeout, or no output.
    pub down: &'static str,
    /// Why it is not ready when its context's metadata file is missing.
    pub no_context: &'static str,
    /// A needle in the probe's last line, and why it is not ready then.
    pub known: &'static [(&'static str, &'static str)],
}

/// A tool's own cleanup command. `{name}` in `args` is replaced by the
/// finding's directory name (toolchain versions), `{path}` in `args` and in
/// `env`'s values by the finding's path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NativeCommand {
    pub program: &'static str,
    pub args: &'static [&'static str],
    pub location: Option<Location>,
    /// Run once before the commands of an item; `None` runs them at once.
    pub ready: Option<ReadyProbe>,
    /// Set for the command only, not for its location probe.
    pub env: &'static [(&'static str, &'static str)],
}

impl NativeCommand {
    /// The arguments for the finding at `path`.
    pub fn args_for(&self, path: &std::path::Path) -> Vec<String> {
        self.args.iter().map(|a| substitute(a, path)).collect()
    }

    /// The environment for the finding at `path`.
    pub fn env_for(&self, path: &std::path::Path) -> Vec<(String, String)> {
        self.env.iter().map(|(k, v)| ((*k).to_owned(), substitute(v, path))).collect()
    }
}

fn substitute(word: &str, path: &std::path::Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    word.replace("{name}", &name).replace("{path}", &path.to_string_lossy())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    Delete,
    /// Delete after making directories writable (Go module cache is read-only).
    DeleteWritable,
    Trash,
    /// Leave it, `tool-not-ready`: without its tool the folder is not safe to
    /// remove (a conda package cache, whose packages environments may link to).
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Removal {
    /// Rename to a tombstone, then delete without following symlinks.
    Delete,
    /// Move to the macOS Trash.
    Trash,
    /// The tool's command when it is installed and reports this location; otherwise `fallback`.
    Native { cmd: NativeCommand, fallback: Fallback },
    /// Only these commands, only with the runtime running; the directory is never deleted or trashed.
    NativeOnly(&'static [NativeCommand]),
    /// Report only.
    None,
}

impl Removal {
    /// Whether the executor may remove files of the finding itself.
    pub fn removes_files(&self) -> bool {
        matches!(self, Removal::Delete | Removal::Trash | Removal::Native { .. })
    }
}
