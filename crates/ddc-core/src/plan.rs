//! Cleanup plan: which findings may be cleaned and how (ADR "Cleanup safety
//! model", §1 and §5). Building a plan touches nothing.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::class::{Category, Fallback, NativeCommand, Removal, RiskClass};
use crate::exec::SkipReason;
use crate::model::{Finding, ScanReport};
use crate::rules::{Catalog, Rule};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Tombstone rename, then delete without following symlinks.
    Delete {
        make_writable: bool,
    },
    Trash,
    /// The tool's command if installed and it reports this location, else `fallback`.
    Native {
        cmd: NativeCommand,
        fallback: Fallback,
    },
    /// Only these commands; never delete or trash.
    NativeOnly(&'static [NativeCommand]),
}

impl Action {
    pub fn name(&self) -> &'static str {
        self.kind().as_str()
    }

    pub fn kind(&self) -> ActionKind {
        match self {
            Action::Delete { .. } => ActionKind::Delete,
            Action::Trash => ActionKind::Trash,
            Action::Native { .. } => ActionKind::Native,
            Action::NativeOnly(_) => ActionKind::NativeOnly,
        }
    }
}

/// An action as the app and the clean report name it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ActionKind {
    Delete,
    Trash,
    /// The tool's own command, or the rule's fallback where the tool is missing
    /// or reports another location.
    Native,
    /// Only the tool's commands.
    NativeOnly,
}

impl ActionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ActionKind::Delete => "delete",
            ActionKind::Trash => "trash",
            ActionKind::Native => "native",
            ActionKind::NativeOnly => "native-only",
        }
    }
}

/// The one derivation of an item's action from its rule, its effective class
/// and a permanent delete chosen for it. `None`: nothing ddc may do (user
/// data, a report-only rule, a runtime ddc has no command for).
pub fn action_for(rule: &Rule, class: RiskClass, permanent: bool) -> Option<Action> {
    if class == RiskClass::UserData || rule.class == RiskClass::UserData {
        return None;
    }
    Some(match rule.removal {
        Removal::None | Removal::NativeOnly([]) => return None,
        // Downgraded to review (no lock file, app-owned): Trash, like any review item.
        Removal::Delete if class == RiskClass::Review && !permanent => Action::Trash,
        Removal::Delete => Action::Delete { make_writable: false },
        Removal::Trash if permanent => Action::Delete { make_writable: false },
        Removal::Trash => Action::Trash,
        Removal::Native { cmd, fallback } => Action::Native { cmd, fallback },
        Removal::NativeOnly(cmds) => Action::NativeOnly(cmds),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanItem {
    pub finding: Finding,
    pub action: Action,
    /// Selected by the scan's defaults (age, lock file, class).
    pub default_selected: bool,
    /// Current choice (defaults, then the user's edits).
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub roots: Vec<PathBuf>,
    pub items: Vec<PlanItem>,
}

#[derive(Debug, Clone, Default)]
pub struct PlanOptions {
    /// Only these classes (empty = every plannable class).
    pub classes: Vec<RiskClass>,
    /// Only these categories (empty = all).
    pub categories: Vec<Category>,
    pub min_size: u64,
    /// `review` items are deleted instead of moved to the Trash.
    pub permanent: bool,
}

/// User data, report-only rules, findings `settings.json` hides and the scan
/// roots themselves never enter a plan.
pub fn build_plan(report: &ScanReport, catalog: &Catalog, opts: &PlanOptions) -> Plan {
    let items = report
        .findings
        .iter()
        .filter(|f| f.class != RiskClass::UserData)
        .filter(|f| !f.ignored)
        .filter(|f| !report.scan_roots.contains(&f.path))
        .filter(|f| opts.classes.is_empty() || opts.classes.contains(&f.class))
        .filter(|f| opts.categories.is_empty() || opts.categories.contains(&f.category))
        .filter(|f| f.bytes >= opts.min_size)
        .filter_map(|f| {
            // Nothing ddc can run (colima, lima, …) is manual only, not planned.
            let action = action_for(catalog.by_id(&f.rule)?, f.class, opts.permanent)?;
            Some(PlanItem { finding: f.clone(), action, default_selected: f.selected, selected: f.selected })
        })
        .collect();
    Plan { roots: report.scan_roots.clone(), items }
}

/// `clean --items`: the items the user confirmed in the app (ADR "Menu bar
/// app: Swift shell over the ddc CLI", §6 item 1). The list is the selection:
/// no class filter, no default selection, no prompt (§7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListedItems {
    /// [`LISTED_SCHEMA_VERSION`].
    pub schema_version: u32,
    pub items: Vec<ListedItem>,
}

pub const LISTED_SCHEMA_VERSION: u32 = 1;

/// One finding of the reviewed scan, as the app shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListedItem {
    /// Absolute and normalised; compared byte for byte, never resolved.
    pub path: PathBuf,
    pub dev: u64,
    pub ino: u64,
    pub rule: String,
    pub class: RiskClass,
    /// The effective action: a `review` item deleted permanently is `delete`.
    pub action: ActionKind,
    /// Delete a `review` item instead of moving it to the Trash. Required on,
    /// and only allowed on, a `review` item listed as `delete`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub permanent: bool,
}

/// Why a list is refused before anything is touched (exit 2).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ListError {
    #[error("cannot read the item list: {0}")]
    Syntax(String),
    #[error("item list schema_version {0} is not {LISTED_SCHEMA_VERSION}")]
    Version(u32),
    #[error("the item list is empty")]
    Empty,
    #[error("not an absolute, normalised path: {0}")]
    Path(PathBuf),
    #[error("listed twice: {0}")]
    Duplicate(PathBuf),
    #[error("permanent is only for review items listed as delete, and required there: {0}")]
    Permanent(PathBuf),
}

impl ListedItems {
    /// Parses and checks a list without the disk or the catalog.
    pub fn parse(json: &[u8]) -> Result<Self, ListError> {
        let list: Self = serde_json::from_slice(json).map_err(|e| ListError::Syntax(e.to_string()))?;
        list.validate()?;
        Ok(list)
    }

    pub fn validate(&self) -> Result<(), ListError> {
        if self.schema_version != LISTED_SCHEMA_VERSION {
            return Err(ListError::Version(self.schema_version));
        }
        if self.items.is_empty() {
            return Err(ListError::Empty);
        }
        let mut seen = HashSet::new();
        for item in &self.items {
            if !normalised(&item.path) {
                return Err(ListError::Path(item.path.clone()));
            }
            if !seen.insert(item.path.as_os_str()) {
                return Err(ListError::Duplicate(item.path.clone()));
            }
            let review_delete = item.class == RiskClass::Review && item.action == ActionKind::Delete;
            if item.permanent != review_delete {
                return Err(ListError::Permanent(item.path.clone()));
            }
        }
        Ok(())
    }
}

/// Absolute, without `.`, `..`, repeated or trailing separators: the path
/// equals its own components joined again.
fn normalised(path: &Path) -> bool {
    path.is_absolute()
        && path.components().all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
        && path.components().collect::<PathBuf>().as_os_str() == path.as_os_str()
}

/// A listed item the fresh scan does not confirm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    pub item: ListedItem,
    pub reason: SkipReason,
}

/// The plan of `clean --items`: confirmed items in list order, selected,
/// and the refused ones with their reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedPlan {
    pub plan: Plan,
    pub refused: Vec<Refused>,
}

/// Matches each listed item against the fresh scan. An item runs only if a
/// finding has the same path, `(dev, ino)`, rule and class, and its action
/// derived again equals the listed one; otherwise it is `changed-since-scan`,
/// or `action-not-allowed` where the rule allows no action. An unknown rule
/// is `rule-mismatch` and an item now kept (`.ddc-keep`, `--exclude`) at or
/// above it is `kept`, the reasons the dry run gives. Default selection and
/// switched-off rules are never consulted: the list is the selection.
pub fn plan_listed(report: &ScanReport, catalog: &Catalog, listed: &[ListedItem]) -> ListedPlan {
    let mut items = Vec::new();
    let mut refused = Vec::new();
    for item in listed {
        let fresh = report.findings.iter().find(|f| {
            f.path.as_os_str() == item.path.as_os_str()
                && (f.dev, f.ino) == (item.dev, item.ino)
                && f.rule == item.rule
                && f.class == item.class
        });
        let Some(f) = fresh else {
            let kept = report.findings.iter().any(|f| f.rule == crate::rules::KEPT && item.path.starts_with(&f.path));
            let reason = match catalog.by_id(&item.rule) {
                None => SkipReason::RuleMismatch,
                Some(_) if kept => SkipReason::Kept,
                Some(_) => SkipReason::ChangedSinceScan,
            };
            refused.push(Refused { item: item.clone(), reason });
            continue;
        };
        let Some(action) = catalog.by_id(&f.rule).and_then(|rule| action_for(rule, f.class, item.permanent)) else {
            refused.push(Refused { item: item.clone(), reason: SkipReason::ActionNotAllowed });
            continue;
        };
        if action.kind() != item.action {
            refused.push(Refused { item: item.clone(), reason: SkipReason::ChangedSinceScan });
            continue;
        }
        items.push(PlanItem { finding: f.clone(), action, default_selected: f.selected, selected: true });
    }
    ListedPlan { plan: Plan { roots: report.scan_roots.clone(), items }, refused }
}
