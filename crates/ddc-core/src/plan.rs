//! Cleanup plan: which findings may be cleaned and how (ADR "Cleanup safety
//! model", §1 and §5). Building a plan touches nothing.

use crate::platform::Canonical as _;
use std::borrow::Cow;
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
use crate::rules::{Catalog, KEPT, Rule, worktree};

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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub roots: Vec<PathBuf>,
    pub items: Vec<PlanItem>,
    /// Findings left out of `items`, and why.
    pub dropped: Vec<Dropped>,
}

/// A finding a plan leaves out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dropped {
    pub finding: Finding,
    pub reason: DropReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropReason {
    /// It lies inside a selected linked worktree, which `git worktree
    /// remove` removes with it (ADR "Agent footprint", §2): its bytes are
    /// the worktree item's.
    Contained { worktree: PathBuf },
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
    let (items, dropped) = contain(items, &report.findings);
    Plan { roots: report.scan_roots.clone(), items, dropped }
}

/// The findings strictly inside `dir`.
fn inside<'f>(findings: &'f [Finding], dir: &'f Path) -> impl Iterator<Item = &'f Finding> + 'f {
    findings.iter().filter(move |f| f.path != dir && f.path.starts_with(dir))
}

/// The items inside a selected linked worktree leave the plan as
/// `contained`: `git worktree remove` removes them with it. The worktree
/// item's bytes then hold every finding inside it, whose bytes a region's
/// own finding never counted, so the plan's total counts each byte once.
fn contain(items: Vec<PlanItem>, findings: &[Finding]) -> (Vec<PlanItem>, Vec<Dropped>) {
    let worktrees: Vec<PathBuf> = items
        .iter()
        .filter(|i| i.selected && i.finding.rule == worktree::WORKTREE_RULE)
        .map(|i| i.finding.path.clone())
        .collect();
    if worktrees.is_empty() {
        return (items, Vec::new());
    }
    // The outermost selected worktree holding `path`.
    let within = |path: &Path| {
        worktrees
            .iter()
            .filter(|w| path != w.as_path() && path.starts_with(w))
            .min_by_key(|w| w.as_os_str().len())
            .cloned()
    };
    let (mut kept, mut dropped) = (Vec::new(), Vec::new());
    for item in items {
        match within(&item.finding.path) {
            Some(worktree) => {
                dropped.push(Dropped { finding: item.finding, reason: DropReason::Contained { worktree } })
            }
            None => kept.push(item),
        }
    }
    // A worktree inside another one left with what it holds.
    for item in kept.iter_mut().filter(|i| worktrees.contains(&i.finding.path)) {
        let wt = &mut item.finding;
        for inner in inside(findings, &wt.path).filter(|f| within(&f.path).as_deref() == Some(wt.path.as_path())) {
            wt.bytes = wt.bytes.saturating_add(inner.bytes);
            wt.shared_bytes = wt.shared_bytes.saturating_add(inner.shared_bytes);
            wt.private_bytes = wt.private_bytes.zip(inner.private_bytes).map(|(a, b)| a.saturating_add(b));
            wt.files = wt.files.saturating_add(inner.files);
            wt.dirs = wt.dirs.saturating_add(inner.dirs);
        }
    }
    (kept, dropped)
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
    plan_listed_checked(report, entries, catalog, listed, procs, skips, None)
}

/// [`plan_listed_with`], and a listed linked worktree's conditions 2, 4, 5
/// and 6 run through `worktrees`' git (decision 6 of story "Worktree
/// findings and provenance"): a person's list waives 1 and 3, never the
/// rest, so the plan already refuses what the executor would keep, as
/// `worktree-kept`. Its class is the conditions' verdict, which the list
/// answers for: `review` and `rebuildable` match each other. An item
/// inside a planned worktree is refused `inside-another-item`: git removes
/// it with the worktree. With `worktrees` `None` only the executor checks
/// the conditions.
#[allow(clippy::too_many_arguments)]
pub fn plan_listed_checked(
    report: &ScanReport,
    entries: &EntryIndex,
    catalog: &Catalog,
    listed: &[ListedItem],
    procs: &dyn ProcessTable,
    skips: &[PathBuf],
    worktrees: Option<&worktree::Recheck<'_>>,
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
        let Some(f) = fresh(report, item) else {
            refused.push(Refused { item: item.clone(), reason: unconfirmed(report, catalog, item) });
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
        if f.rule == worktree::WORKTREE_RULE
            && let Some(rc) = worktrees
        {
            let main = f.project.as_ref().map(|p| p.root.as_path());
            let owners = || Cow::Owned(removable_findings(&report.findings, &f.path));
            let checked = match main {
                Some(main) => worktree::recheck(&f.path, Some(main), true, rc, &owners).map(|_| ()),
                None => Err(SkipReason::ChangedSinceScan),
            };
            if let Err(reason) = checked {
                refused.push(Refused { item: item.clone(), reason });
                continue;
            }
        }
        items.push(PlanItem { finding: f.clone(), action, default_selected: f.selected, selected: true });
    }
    // What git removes with a listed worktree is not removed again.
    let worktrees: Vec<PathBuf> =
        items.iter().filter(|i| i.finding.rule == worktree::WORKTREE_RULE).map(|i| i.finding.path.clone()).collect();
    let (inner, items): (Vec<PlanItem>, Vec<PlanItem>) = items
        .into_iter()
        .partition(|i| worktrees.iter().any(|w| i.finding.path != *w && i.finding.path.starts_with(w)));
    for planned in inner {
        if let Some(item) = listed.iter().find(|l| l.path == planned.finding.path) {
            refused.push(Refused { item: item.clone(), reason: SkipReason::InsideAnotherItem });
        }
    }
    ListedPlan { plan: Plan { roots: report.scan_roots.clone(), items, dropped: Vec::new() }, refused }
}

/// The finding of the fresh scan a listed item names: the same path,
/// `(dev, ino)`, rule and class (a worktree's two verdicts match).
fn fresh<'r>(report: &'r ScanReport, item: &ListedItem) -> Option<&'r Finding> {
    report.findings.iter().find(|f| {
        f.path.as_os_str() == item.path.as_os_str()
            && (f.dev, f.ino) == (item.dev, item.ino)
            && f.rule == item.rule
            && (f.class == item.class || worktree_verdicts(&f.rule, f.class, item.class))
    })
}

/// Why the fresh scan does not confirm a listed item: an unknown rule, an
/// item kept now at or above it, or anything else that changed.
fn unconfirmed(report: &ScanReport, catalog: &Catalog, item: &ListedItem) -> SkipReason {
    let kept = report.findings.iter().any(|f| f.rule == crate::rules::KEPT && item.path.starts_with(&f.path));
    match catalog.by_id(&item.rule) {
        None => SkipReason::RuleMismatch,
        Some(_) if kept => SkipReason::Kept,
        Some(_) => SkipReason::ChangedSinceScan,
    }
}

/// The tree a coding agent's clean is bound to (ADR "Agent footprint", §7,
/// and its note of 2026-11-14): the main checkout and the linked worktrees
/// the walk recognised as this project's. It is the owner's consent rule
/// for an honest agent, not a defence against one that removes files itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentBound {
    /// The main checkout (or the bare repository): the journal's `project`.
    pub project: PathBuf,
    /// The checkouts inside which an agent may remove `safe` items.
    pub checkouts: Vec<PathBuf>,
    /// Checkouts git listed that are out of the bound, and why in words.
    pub left_out: Vec<(PathBuf, String)>,
}

impl AgentBound {
    /// From the walk of the project's checkouts: `main` unless `bare`, and
    /// each of `checkouts` the report holds as a `git.worktree` finding of
    /// `main` at that exact path (one hidden by `min_size` or a disabled
    /// rule included). A checkout at or above `home`, by identity, or whose
    /// parent is another account's, is left out.
    pub fn new(report: &ScanReport, main: &Path, bare: bool, checkouts: &[PathBuf], home: &Path) -> Self {
        let mut bound = AgentBound { project: main.to_path_buf(), checkouts: Vec::new(), left_out: Vec::new() };
        for checkout in checkouts {
            let is_main = !bare && checkout == main;
            let recognised = is_main
                || report.findings.iter().any(|f| {
                    f.path == *checkout
                        && f.rule == worktree::WORKTREE_RULE
                        && f.project.as_ref().is_some_and(|p| p.root == main)
                });
            let why = if !recognised {
                Some("the walk did not find it as a worktree of this project")
            } else if at_or_above(checkout, home) {
                Some("it is at or above the home folder")
            } else if checkout.parent().is_some_and(foreign) {
                Some("its folder belongs to another account")
            } else {
                None
            };
            match why {
                Some(why) => bound.left_out.push((checkout.clone(), why.to_owned())),
                None => bound.checkouts.push(checkout.clone()),
            }
        }
        bound
    }

    /// Strictly inside one of the bound's checkouts; never a checkout itself.
    pub fn contains(&self, path: &Path) -> bool {
        self.checkouts.iter().any(|c| path != c && path.starts_with(c))
    }

    /// `path` holds a checkout git listed, in the bound or left out of it:
    /// removing it would remove the checkout too.
    fn holds_a_checkout(&self, path: &Path) -> bool {
        self.checkouts.iter().chain(self.left_out.iter().map(|(c, _)| c)).any(|c| c.starts_with(path))
    }
}

/// Another account's folder, or one whose owner cannot be read: `st_uid`
/// on macOS and Linux ([`crate::platform::meta::uid`], which only ever
/// narrows the bound here), and under an elevated Windows token the
/// platform's own check of a root.
fn foreign(dir: &Path) -> bool {
    if crate::platform::require_own_root(dir).is_err() {
        return true;
    }
    let me = crate::platform::procs::uid();
    match std::fs::metadata(dir) {
        Ok(md) => crate::platform::meta::uid(&md).is_some_and(|owner| owner != me),
        Err(_) => true,
    }
}

/// `dir` is the home folder or one of its ancestors, by identity where both
/// can be read (a symlinked home included).
fn at_or_above(dir: &Path, home: &Path) -> bool {
    use crate::platform::Canonical;
    if home.starts_with(dir) {
        return true;
    }
    let id = |p: &Path| crate::platform::meta::stat(p).ok().map(|s| (s.dev, s.ino));
    let Some(dir_id) = id(dir) else { return false };
    // The home's own path and its resolved one: a link above it (macOS's
    // `/var` to `/private/var`) hides the real ancestors.
    let canonical = home.canonical().ok();
    std::iter::once(home).chain(canonical.as_deref()).any(|h| h.ancestors().any(|a| id(a) == Some(dir_id)))
}

/// Whether a coding agent may remove `f` with `action` inside `bound`
/// (ADR "Agent footprint", §7): a `safe` project artifact by its rule and
/// its class, selected by default and not hidden, deleted, strictly inside
/// one of the checkouts, holding none, and not kept. The plan and the
/// executor both ask.
pub fn agent_may_remove(f: &Finding, action: &Action, bound: &AgentBound) -> bool {
    f.class == RiskClass::Safe
        && f.rule_class == RiskClass::Safe
        && f.selected
        && !f.ignored
        && f.category == Category::ProjectArtifact
        && matches!(action, Action::Delete { .. })
        && f.rule != KEPT
        && bound.contains(&f.path)
        && !bound.holds_a_checkout(&f.path)
}

/// One item of an agent's clean: what it is, and whether it goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentItem {
    pub item: ListedItem,
    pub bytes: u64,
    pub verdict: AgentVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentVerdict {
    /// In the plan the executor runs.
    Remove,
    /// Left alone, with why.
    Skip(SkipReason),
}

/// The plan of `ddc agent clean`: the executor's plan (the items that go),
/// every item considered with its verdict, and what a person may clean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPlan {
    pub plan: Plan,
    pub items: Vec<AgentItem>,
    pub for_person: Vec<ListedItem>,
}

/// An agent's plan over the fresh walk of its project (ADR "Agent
/// footprint", §7). Without `listed`, every finding inside the bound that
/// `build_plan`'s filters let through is considered, by path; with it, the
/// list filters and never widens, in its order. A `safe` item an agent may
/// remove goes when `opt_in` is on and is `agent-opt-in-off` otherwise;
/// any other is `not-consented`. `contain()` is not applied: an agent never
/// removes a worktree, so a safe item inside one is its own.
///
/// `for_person`: the `rebuildable` items selected by default inside the
/// bound, and while `opt_in` is off the `safe` ones too; nothing inside a
/// listed worktree. Under `listed`, only listed items.
pub fn plan_agent(
    report: &ScanReport,
    catalog: &Catalog,
    bound: &AgentBound,
    listed: Option<&[ListedItem]>,
    opt_in: bool,
) -> AgentPlan {
    let plannable = |f: &Finding| -> Option<Action> {
        if f.class == RiskClass::UserData || f.ignored || report.scan_roots.contains(&f.path) {
            return None;
        }
        let rule = catalog.by_id(&f.rule).filter(|r| !r.listed_only)?;
        action_for(rule, f.class, false)
    };
    let as_listed = |f: &Finding, action: &Action| ListedItem {
        path: f.path.clone(),
        dev: f.dev,
        ino: f.ino,
        rule: f.rule.clone(),
        class: f.class,
        action: action.kind(),
        permanent: false,
    };
    let verdict = |f: &Finding, action: &Action| {
        if !agent_may_remove(f, action, bound) {
            AgentVerdict::Skip(SkipReason::NotConsented)
        } else if opt_in {
            AgentVerdict::Remove
        } else {
            AgentVerdict::Skip(SkipReason::AgentOptInOff)
        }
    };
    // Considered: inside the bound, and its worktrees themselves (never removed
    // by an agent, listed for the person).
    let in_tree = |p: &Path| bound.contains(p) || bound.checkouts.iter().any(|c| c == p && *c != bound.project);
    let mut considered: Vec<(Option<&Finding>, Option<Action>, AgentItem)> = Vec::new();
    match listed {
        None => {
            let mut found: Vec<&Finding> = report.findings.iter().filter(|f| in_tree(&f.path)).collect();
            found.sort_by(|a, b| a.path.cmp(&b.path));
            for f in found {
                let Some(action) = plannable(f) else { continue };
                let item = AgentItem { item: as_listed(f, &action), bytes: f.bytes, verdict: verdict(f, &action) };
                considered.push((Some(f), Some(action), item));
            }
        }
        Some(list) => {
            for listed in list {
                let skip = |reason| AgentItem { item: listed.clone(), bytes: 0, verdict: AgentVerdict::Skip(reason) };
                if !in_tree(&listed.path) {
                    considered.push((None, None, skip(SkipReason::NotConsented)));
                    continue;
                }
                let Some(f) = fresh(report, listed) else {
                    considered.push((None, None, skip(unconfirmed(report, catalog, listed))));
                    continue;
                };
                let Some(action) = plannable(f) else {
                    considered.push((Some(f), None, skip(SkipReason::NotConsented)));
                    continue;
                };
                let mut v = verdict(f, &action);
                // A safe item the agent may remove, listed with another action: changed.
                if v != AgentVerdict::Skip(SkipReason::NotConsented) && listed.action != action.kind() {
                    v = AgentVerdict::Skip(SkipReason::ChangedSinceScan);
                }
                considered.push((
                    Some(f),
                    Some(action),
                    AgentItem { item: as_listed(f, &action), bytes: f.bytes, verdict: v },
                ));
            }
        }
    }
    let plan_items = considered
        .iter()
        .filter(|(_, _, i)| i.verdict == AgentVerdict::Remove)
        .filter_map(|(f, action, _)| {
            Some(PlanItem { finding: (*f)?.clone(), action: (*action)?, default_selected: true, selected: true })
        })
        .collect();
    // What a person may clean: the rebuildable ones selected by default, and
    // the safe ones the opt-in held back; nothing a listed worktree holds.
    let mut for_person: Vec<ListedItem> = considered
        .iter()
        .filter_map(|(f, action, i)| {
            let (f, action) = ((*f)?, (*action)?);
            let rebuildable = f.class == RiskClass::Rebuildable && f.selected && !f.ignored;
            let held = i.verdict == AgentVerdict::Skip(SkipReason::AgentOptInOff);
            (rebuildable || held).then(|| as_listed(f, &action))
        })
        .collect();
    let worktrees: Vec<PathBuf> =
        for_person.iter().filter(|i| i.rule == worktree::WORKTREE_RULE).map(|i| i.path.clone()).collect();
    for_person.retain(|i| !worktrees.iter().any(|w| i.path != *w && i.path.starts_with(w)));
    AgentPlan {
        plan: Plan { roots: report.scan_roots.clone(), items: plan_items, dropped: Vec::new() },
        items: considered.into_iter().map(|(_, _, i)| i).collect(),
        for_person,
    }
}

/// A linked worktree's class is the six conditions' verdict: under a
/// person's list `review` and `rebuildable` name the same item.
fn worktree_verdicts(rule: &str, fresh: RiskClass, listed: RiskClass) -> bool {
    let verdict = |c| matches!(c, RiskClass::Review | RiskClass::Rebuildable);
    rule == worktree::WORKTREE_RULE && verdict(fresh) && verdict(listed)
}

/// The findings inside `dir` ddc removes on its own (`safe` or
/// `rebuildable`, removed by `delete`), as the scan's accounting of a
/// worktree's ignored content counts them.
fn removable_findings(findings: &[Finding], dir: &Path) -> Vec<PathBuf> {
    let mut owners: Vec<PathBuf> = inside(findings, dir)
        .filter(|f| f.rule != KEPT && matches!(f.class, RiskClass::Safe | RiskClass::Rebuildable))
        .filter(|f| f.removal == RemovalKind::Delete)
        .map(|f| f.path.clone())
        .collect();
    owners.sort();
    owners
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
