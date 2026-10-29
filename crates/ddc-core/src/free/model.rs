//! `ddc disk free --actions` and its report (schema `docs/disk-free.schema.json`).

use std::collections::HashSet;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::brew::NeededBy;
use crate::disk::ActionId;

/// Version of [`ActionList`] and of [`FreeReport`].
pub const FREE_SCHEMA_VERSION: u32 = 3;

/// The actions a person confirmed, as the app's sheet passes them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActionList {
    /// [`FREE_SCHEMA_VERSION`].
    pub schema_version: u32,
    pub actions: Vec<ListedAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListedAction {
    pub action: ActionId,
    /// As the disk report lists it: the runtime's asset id of
    /// `simctl-runtime-delete`, the environment's path of
    /// `conda-env-remove`, the install's prefix of `conda-clean`; as `ddc
    /// disk brew-packages` lists it: the formula's `full_name` of
    /// `brew-uninstall`. Required on those four, refused on `brew-cleanup`
    /// and `brew-uninstall-orphans`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    /// The versions of the formula's kegs the person saw: `brew-uninstall`
    /// is refused when brew lists others right before it runs. Required on
    /// `brew-uninstall` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kegs: Option<Vec<String>>,
    /// The formulae, by `full_name`, the person confirmed as needed by
    /// nothing. Required on `brew-uninstall-orphans` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub names: Option<Vec<String>>,
}

/// Why a list is refused before anything runs (exit 2).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActionListError {
    #[error("cannot read the action list: {0}")]
    Syntax(String),
    #[error("action list schema_version {0} is not {FREE_SCHEMA_VERSION}")]
    Version(u32),
    #[error("the action list is empty")]
    Empty,
    #[error("listed twice: {0}")]
    Duplicate(String),
    #[error("{} takes no identifier", .0.as_str())]
    UnexpectedIdentifier(ActionId),
    /// A runtime's id, an environment's path, an install's prefix or a
    /// formula's `full_name`.
    #[error("{} needs its identifier", .0.as_str())]
    MissingIdentifier(ActionId),
    #[error("brew-uninstall needs the versions of its kegs")]
    MissingKegs,
    #[error("brew-uninstall-orphans needs the names of the formulae")]
    MissingNames,
    #[error("{} takes no kegs", .0.as_str())]
    KegsMisplaced(ActionId),
    #[error("{} takes no names", .0.as_str())]
    NamesMisplaced(ActionId),
    /// Empty, or read by brew as an option.
    #[error("not a formula or a version: {0:?}")]
    BadName(String),
}

impl ActionList {
    /// Parses and checks a list without running anything.
    pub fn parse(json: &[u8]) -> Result<Self, ActionListError> {
        let list: Self = serde_json::from_slice(json).map_err(|e| ActionListError::Syntax(e.to_string()))?;
        list.validate()?;
        Ok(list)
    }

    pub fn validate(&self) -> Result<(), ActionListError> {
        if self.schema_version != FREE_SCHEMA_VERSION {
            return Err(ActionListError::Version(self.schema_version));
        }
        if self.actions.is_empty() {
            return Err(ActionListError::Empty);
        }
        let mut seen = HashSet::new();
        for a in &self.actions {
            a.validate()?;
            if !seen.insert((a.action, a.identifier.clone())) {
                return Err(ActionListError::Duplicate(a.name()));
            }
        }
        Ok(())
    }
}

/// Each of `words` a word brew reads as a name, listed once.
fn plain_words(words: &[String]) -> Result<(), ActionListError> {
    let mut seen = HashSet::new();
    for w in words {
        if w.is_empty() || w.starts_with('-') {
            return Err(ActionListError::BadName(w.clone()));
        }
        if !seen.insert(w) {
            return Err(ActionListError::Duplicate(w.clone()));
        }
    }
    Ok(())
}

impl ListedAction {
    /// An action with no kegs and no names.
    pub fn new(action: ActionId, identifier: Option<String>) -> Self {
        Self { action, identifier, kegs: None, names: None }
    }

    /// Each field the action needs, and none it does not.
    fn validate(&self) -> Result<(), ActionListError> {
        let action = self.action;
        match (action, &self.identifier) {
            (ActionId::BrewCleanup | ActionId::BrewUninstallOrphans, Some(_)) => {
                return Err(ActionListError::UnexpectedIdentifier(action));
            }
            (
                ActionId::SimctlRuntimeDelete
                | ActionId::CondaEnvRemove
                | ActionId::CondaClean
                | ActionId::BrewUninstall,
                None,
            ) => return Err(ActionListError::MissingIdentifier(action)),
            (ActionId::BrewUninstall, Some(name)) => plain_words(std::slice::from_ref(name))?,
            _ => {}
        }
        match (action, &self.kegs) {
            (ActionId::BrewUninstall, None) => return Err(ActionListError::MissingKegs),
            (ActionId::BrewUninstall, Some(kegs)) if kegs.is_empty() => return Err(ActionListError::MissingKegs),
            (ActionId::BrewUninstall, Some(kegs)) => plain_words(kegs)?,
            (_, Some(_)) => return Err(ActionListError::KegsMisplaced(action)),
            (_, None) => {}
        }
        match (action, &self.names) {
            (ActionId::BrewUninstallOrphans, None) => return Err(ActionListError::MissingNames),
            (ActionId::BrewUninstallOrphans, Some(names)) if names.is_empty() => {
                return Err(ActionListError::MissingNames);
            }
            (ActionId::BrewUninstallOrphans, Some(names)) => plain_words(names)?,
            (_, Some(_)) => return Err(ActionListError::NamesMisplaced(action)),
            (_, None) => {}
        }
        Ok(())
    }

    /// `brew-cleanup`, `simctl-runtime-delete <identifier>`.
    pub fn name(&self) -> String {
        match &self.identifier {
            Some(id) => format!("{} {id}", self.action.as_str()),
            None => self.action.as_str().to_owned(),
        }
    }
}

impl ActionId {
    pub fn as_str(self) -> &'static str {
        match self {
            ActionId::SimctlRuntimeDelete => "simctl-runtime-delete",
            ActionId::BrewCleanup => "brew-cleanup",
            ActionId::CondaEnvRemove => "conda-env-remove",
            ActionId::CondaClean => "conda-clean",
            ActionId::BrewUninstall => "brew-uninstall",
            ActionId::BrewUninstallOrphans => "brew-uninstall-orphans",
        }
    }

    /// The disk report's area the action frees.
    pub fn area(self) -> &'static str {
        match self {
            ActionId::SimctlRuntimeDelete => "simulator-runtimes",
            ActionId::BrewCleanup | ActionId::BrewUninstall | ActionId::BrewUninstallOrphans => "homebrew",
            ActionId::CondaEnvRemove | ActionId::CondaClean => "conda",
        }
    }
}

/// What `ddc disk free` did, one report per listed action in list order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FreeReport {
    pub schema_version: u32,
    pub ddc_version: String,
    pub dry_run: bool,
    pub actions: Vec<ActionReport>,
    /// `actions.jsonl`; absent from a dry run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal: Option<PathBuf>,
    /// `last-scan.json` after a `brew-cleanup` that ran.
    pub last_scan: ReportEdit,
    /// `last-disk.json` after any action that ran.
    pub last_disk: ReportEdit,
}

impl FreeReport {
    pub fn new(dry_run: bool, actions: Vec<ActionReport>) -> Self {
        Self {
            schema_version: FREE_SCHEMA_VERSION,
            ddc_version: env!("CARGO_PKG_VERSION").to_owned(),
            dry_run,
            actions,
            journal: None,
            last_scan: ReportEdit::Unchanged,
            last_disk: ReportEdit::Unchanged,
        }
    }
}

/// What happened to a saved report after the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ReportEdit {
    Edited,
    /// Nothing that ran touched it, or a dry run.
    Unchanged,
    /// Absent, or of another schema version or other roots: left alone.
    Skipped,
    /// It could not be written; the run's own report is still right.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ActionReport {
    pub action: ActionId,
    /// The disk report's area: `simulator-runtimes`, `homebrew` or `conda`.
    pub area: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    /// The program and its arguments, as run (or as it would run).
    pub command: Vec<String>,
    /// `NAME=value`, added to the environment.
    pub env: Vec<String>,
    /// The same command for a person to paste into Terminal.
    pub terminal_command: String,
    pub result: ActionResult,
    /// Allocated bytes of the folders the action frees; absent when they
    /// were not measured (a skip in `settings.json`, or refused first).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_before: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_after: Option<u64>,
    /// The last part of the tool's output.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output: String,
    /// The tool's own dry run; only in a dry run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<Preview>,
    /// The kegs a `brew-uninstall` or `brew-uninstall-orphans` removes, as
    /// brew listed them right before it ran, each measured by ddc.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kegs: Vec<KegReport>,
    /// What still needs a confirmed formula, when that refused it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needed_by: Vec<NeededBy>,
}

/// One keg of a formula brew removes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct KegReport {
    /// The formula's `full_name`.
    pub formula: String,
    pub version: String,
    /// `<cellar>/<name>/<version>`, as brew spells the Cellar.
    pub path: PathBuf,
    /// Allocated bytes; absent under a skip of `settings.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

impl ActionReport {
    /// The action ran its tool (a failure included): the saved reports
    /// no longer describe its area.
    pub fn ran(&self) -> bool {
        matches!(
            self.result,
            ActionResult::Freed
                | ActionResult::RemovedNotFreed
                | ActionResult::StillDeleting
                | ActionResult::Failed { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum ActionResult {
    /// The tool succeeded and, for a runtime, the runtime left the list.
    Freed,
    /// The runtime left the list, and its folder did not shrink; or conda
    /// exited 0 and the environment is still there.
    RemovedNotFreed,
    /// The runtime is still listed after the timeout, or ddc was stopped
    /// while it waited. CoreSimulator's daemon may still finish it.
    StillDeleting,
    /// The location probe refused it: nothing ran.
    Refused { reason: Refusal },
    /// The tool exited non-zero, timed out or was stopped.
    Failed {
        /// The output names a permission error: the Terminal command may
        /// need an administrator.
        needs_admin: bool,
    },
    /// A dry run: the probe passed.
    WouldRun,
    /// A stop arrived before it started.
    NotRun,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Refusal {
    /// No runtime, or more than one, has this identifier.
    UnknownRuntime,
    /// Its `parentMountPath` is not under `/System/Library/AssetsV2/`.
    NotUnderAssets,
    NotDeletable,
    /// Already in the state `Deleting`.
    Deleting,
    /// No `runtimeIdentifier`: its devices cannot be joined to it.
    NoRuntimeIdentifier,
    /// A device on it is not shut down: booted, booting or shutting down.
    Booted,
    /// `simctl list devices -j` failed.
    DevicesUnknown,
    /// Its `path` lies in no `<hash>.asset` folder under AssetsV2.
    NoAssetFolder,
    NoDeveloperTools,
    /// Xcode has not finished its first launch.
    FirstLaunchPending,
    /// `simctl` is missing, or `runtime list` failed.
    SimctlFailed,
    /// No `/opt/homebrew/bin/brew`, or it would not say where it lives.
    BrewMissing,
    /// `brew --prefix` is not the Homebrew area's folder.
    PrefixMismatch,
    /// `brew --cellar` is not inside the prefix.
    CellarOutside,
    /// `brew --cache` is not `~/Library/Caches/Homebrew`.
    CacheMismatch,
    /// The environment lies in the home folder: the Review window cleans it.
    CondaInHome,
    /// Not one of `/opt/miniconda3`, `/opt/anaconda3`, `/opt/miniforge3`
    /// (an environment: not in its `envs` folder), by device and inode; or
    /// a path conda cannot be given (not UTF-8, or holding a comma).
    CondaNotInOpt,
    /// The install has no `bin/conda`, or its `config --show` failed or
    /// printed what ddc cannot read.
    CondaMissing,
    /// conda's `root_prefix` is not the install's folder.
    CondaRootMismatch,
    /// The install's base environment.
    CondaBase,
    /// Its parent is not in conda's `envs_dirs`.
    CondaNotInEnvsDirs,
    /// It holds no `conda-meta/history`.
    CondaNotAnEnvironment,
    /// The install's `pkgs` is not in conda's `pkgs_dirs`.
    CondaPackagesNotListed,
    /// The user cannot write it: conda needs an administrator.
    CondaNotWritable,
    /// A running process uses the environment.
    CondaInUse,
    /// An install that lists the package folder links packages into
    /// environments by symlink.
    CondaSoftlinks,
    /// conda's package folders, set in a `.condarc`, cannot be limited to
    /// this one.
    CondaNotScoped,
    /// brew no longer lists the formula as installed.
    BrewNotInstalled,
    /// The formula is pinned.
    BrewPinned,
    /// brew lists other versions of it than the person confirmed.
    BrewKegsChanged,
    /// An installed formula or cask still needs it.
    BrewNeeded,
    /// A formula confirmed as needed by nothing was installed on request.
    BrewNotOrphan,
    /// A service of the user's is loaded for it.
    BrewServiceRunning,
    /// A root service is registered for it.
    BrewServiceRoot,
    /// The user's service is registered but not running.
    BrewServiceRegistered,
    /// brew listed a formula without a field ddc needs: what needs what
    /// is not known.
    BrewFieldMissing,
}

/// The tool's own dry run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Preview {
    /// Its output, line by line, as printed.
    pub lines: Vec<String>,
    /// What it would free: a runtime's measured folder, or what `brew
    /// cleanup -n` says it would free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub estimate_bytes: Option<u64>,
}
