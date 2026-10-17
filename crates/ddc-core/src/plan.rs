//! Cleanup plan: which findings may be cleaned and how (ADR "Cleanup safety
//! model", §1 and §5). Building a plan touches nothing.

use std::path::PathBuf;

use crate::class::{Category, Fallback, NativeCommand, Removal, RiskClass};
use crate::model::{Finding, ScanReport};
use crate::rules::Catalog;

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
        match self {
            Action::Delete { .. } => "delete",
            Action::Trash => "trash",
            Action::Native { .. } => "native",
            Action::NativeOnly(_) => "native-only",
        }
    }
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

/// User data, report-only rules and the scan roots themselves never enter a plan.
pub fn build_plan(report: &ScanReport, catalog: &Catalog, opts: &PlanOptions) -> Plan {
    let items = report
        .findings
        .iter()
        .filter(|f| f.class != RiskClass::UserData)
        .filter(|f| !report.scan_roots.contains(&f.path))
        .filter(|f| opts.classes.is_empty() || opts.classes.contains(&f.class))
        .filter(|f| opts.categories.is_empty() || opts.categories.contains(&f.category))
        .filter(|f| f.bytes >= opts.min_size)
        .filter_map(|f| {
            let rule = catalog.by_id(&f.rule)?;
            let action = match rule.removal {
                Removal::None => return None,
                // Downgraded to review (no lock file, app-owned): Trash by default, like any review item.
                Removal::Delete if f.class == RiskClass::Review && !opts.permanent => Action::Trash,
                Removal::Delete => Action::Delete { make_writable: false },
                Removal::Trash if opts.permanent => Action::Delete { make_writable: false },
                Removal::Trash => Action::Trash,
                Removal::Native { cmd, fallback } => Action::Native { cmd, fallback },
                // Nothing ddc can run (colima, lima, …): manual only, not planned.
                Removal::NativeOnly([]) => return None,
                Removal::NativeOnly(cmds) => Action::NativeOnly(cmds),
            };
            Some(PlanItem { finding: f.clone(), action, default_selected: f.selected, selected: f.selected })
        })
        .collect();
    Plan { roots: report.scan_roots.clone(), items }
}
