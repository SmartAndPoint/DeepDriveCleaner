//! Cleanup plan: which findings may be cleaned and how (ADR "Cleanup safety
//! model", §1 and §5). Building a plan touches nothing.

use crate::platform::Canonical as _;
use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::agents::HarnessId;
use crate::agents::liveness::{self, HarnessLiveness};
use crate::class::{Category, Fallback, NativeCommand, Removal, RiskClass};
use crate::exec::guard::{agent_live, harness_state};
use crate::exec::{ProcessTable, SkipReason, SystemProcesses};
use crate::model::{Finding, NotSelected, RemovalKind, ScanReport};
use crate::platform::procs::ProcInfo;
use crate::report::contents::{EntryIndex, EntryState, NotAnEntry};
use crate::rules::entries;
use crate::rules::{Catalog, KEPT, Rule};

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
    /// An entry of one of the user's own folders the user picked, to the
    /// Trash (story "User data: pick your own files for the Trash"). Only
    /// [`plan_listed`] makes it; [`action_for`] and [`build_plan`] never do.
    TrashEntry,
}

impl Action {
    pub fn name(&self) -> &'static str {
        self.kind().as_str()
    }

    pub fn kind(&self) -> ActionKind {
        match self {
            Action::Delete { .. } => ActionKind::Delete,
            Action::Trash | Action::TrashEntry => ActionKind::Trash,
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

/// User data, report-only rules, `listed_only` rules, findings
/// `settings.json` hides and the scan roots themselves never enter a plan.
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
            // Only a list a person made holds a `listed_only` rule's item.
            let rule = catalog.by_id(&f.rule).filter(|r| !r.listed_only)?;
            let action = action_for(rule, f.class, opts.permanent)?;
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
    #[serde(serialize_with = "crate::json_path::path")]
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
pub(crate) fn normalised(path: &Path) -> bool {
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
///
/// A `user-data` item is an entry the user picked; `entry` matches it
/// against the fresh scan's full list of entries. Entries come first in the
/// plan, so one runs before a finding inside it.
///
/// A coding agent's folder whose harness is live is `agent-live` (story
/// "Harness data rules"): the system's process table, read once.
pub fn plan_listed(report: &ScanReport, entries: &EntryIndex, catalog: &Catalog, listed: &[ListedItem]) -> ListedPlan {
    plan_listed_with(report, entries, catalog, listed, &SystemProcesses, &[])
}

/// [`plan_listed`] with the process table to read, and the scan's skips
/// (`ScanOptions::skip` and `user_skips`), under which no Codex lock is
/// read. The table is read once, and only when an item of a harness's
/// folder reaches the check; each harness is decided once per plan.
pub fn plan_listed_with(
    report: &ScanReport,
    entries: &EntryIndex,
    catalog: &Catalog,
    listed: &[ListedItem],
    procs: &dyn ProcessTable,
    skips: &[PathBuf],
) -> ListedPlan {
    let table: OnceCell<Vec<ProcInfo>> = OnceCell::new();
    let mut harnesses: HashMap<HarnessId, HarnessLiveness> = HashMap::new();
    let mut harness_of = |h: HarnessId| {
        *harnesses
            .entry(h)
            .or_insert_with(|| harness_state(&report.home, h, table.get_or_init(|| procs.processes()), procs, skips))
    };
    let mut items = Vec::new();
    let mut refused = Vec::new();
    for item in listed.iter().filter(|i| i.class == RiskClass::UserData) {
        match entry(report, entries, catalog, item) {
            Ok(planned) => items.push(planned),
            Err(reason) => refused.push(Refused { item: item.clone(), reason }),
        }
    }
    for item in listed.iter().filter(|i| i.class != RiskClass::UserData) {
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
        if let Some(reason) = liveness::rule_harness(&f.rule).and_then(|h| agent_live(h, harness_of(h))) {
            refused.push(Refused { item: item.clone(), reason });
            continue;
        }
        items.push(PlanItem { finding: f.clone(), action, default_selected: f.selected, selected: true });
    }
    ListedPlan { plan: Plan { roots: report.scan_roots.clone(), items }, refused }
}

/// A picked entry, checked in the order the dry run checks it: the action,
/// the scan's own findings and roots, the parent, the folder, the entry and
/// its identity, then its lock (safety ADR, amendment of 2026-10-20).
fn entry(
    report: &ScanReport,
    index: &EntryIndex,
    catalog: &Catalog,
    item: &ListedItem,
) -> Result<PlanItem, SkipReason> {
    let Some(rule) = catalog.by_id(&item.rule) else { return Err(SkipReason::RuleMismatch) };
    if item.action != ActionKind::Trash || item.permanent {
        return Err(SkipReason::ActionNotAllowed);
    }
    let path = item.path.as_path();
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(SkipReason::ActionNotAllowed);
    };
    // Kept at the entry itself, in one of the folders: a marker at its top or an exclude.
    if index.folders.contains_key(parent)
        && report.findings.iter().any(|f| f.rule == KEPT && f.path.as_os_str() == path.as_os_str())
    {
        return Err(SkipReason::EntryLocked { lock: crate::rules::entries::EntryLock::Kept });
    }
    // The folder itself may not be picked; another finding is no entry.
    if report.findings.iter().any(|f| f.path.as_os_str() == path.as_os_str()) {
        return Err(if catalog.verify(rule, path) { SkipReason::ActionNotAllowed } else { SkipReason::NotAnEntry });
    }
    // The whole folder is kept now.
    if report.findings.iter().any(|f| f.rule == KEPT && parent.starts_with(&f.path)) {
        return Err(SkipReason::Kept);
    }
    if !report.scan_roots.iter().any(|r| path.starts_with(r)) {
        return Err(SkipReason::OutsideScanRoots);
    }
    if parent.canonical().ok().as_deref() != Some(parent) {
        return Err(SkipReason::ParentChanged);
    }
    let Some((folder, found)) = index.entry(parent, name) else { return Err(SkipReason::NotAnEntry) };
    if folder.rule != item.rule || !entries::has_entries(rule.id) {
        return Err(SkipReason::NotAnEntry);
    }
    let Some(found) = found else { return Err(SkipReason::NotAnEntry) };
    match &found.state {
        EntryState::NotAnEntry(NotAnEntry::Symlink) => return Err(SkipReason::Symlink),
        EntryState::NotAnEntry(_) => return Err(SkipReason::NotAnEntry),
        _ => {}
    }
    // Before the lock: a replaced entry is also a new one.
    if (found.dev, found.ino) != (item.dev, item.ino) {
        return Err(SkipReason::ChangedSinceScan);
    }
    if let EntryState::Locked(lock) = &found.state {
        return Err(SkipReason::EntryLocked { lock: lock.clone() });
    }
    let finding = Finding {
        path: item.path.clone(),
        rule: rule.id.to_owned(),
        category: rule.category,
        class: RiskClass::UserData,
        rule_class: rule.class,
        removal: RemovalKind::Trash,
        bytes: found.bytes,
        shared_bytes: 0,
        private_bytes: None,
        files: 0,
        dirs: 0,
        incomplete: found.incomplete,
        project: None,
        selected: false,
        not_selected: Some(NotSelected::UserData),
        ignored: false,
        notes: Vec::new(),
        hint: rule.hint.to_owned(),
        dev: found.dev,
        ino: found.ino,
        parent_dev: Some(folder.dev),
        parent_ino: Some(folder.ino),
        contents: None,
        protection: None,
        modified: None,
        origin: rule.origin,
        worktree: None,
    };
    Ok(PlanItem { finding, action: Action::TrashEntry, default_selected: false, selected: true })
}
