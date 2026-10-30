//! The item list of a load run's `clean --items`: the default plan's items
//! whose own action is `trash` or `delete`, selected or not (a list is the
//! user's selection, and the `review` items are the ones that go to the
//! Trash), so that every one can end trashed or deleted on any machine.
//! Left out:
//! - items removed with a tool (`native`, `native-only`), so no tool runs;
//! - incomplete findings (a locked folder, a path past `PATH_MAX`), which a
//!   clean refuses by design;
//! - items of a rule with an owning app, which a clean skips while that app
//!   runs on the machine.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use ddc_core::class::RiskClass;
use ddc_core::plan::{ActionKind, LISTED_SCHEMA_VERSION, ListedItem, ListedItems};
use ddc_core::rules::Catalog;

use crate::manifest::Manifest;

/// The list for the tree at `tree`, or `None` when the plan has no such item.
pub fn listed(m: &Manifest, tree: &Path) -> anyhow::Result<Option<ListedItems>> {
    // A path the manifest writes with `\xNN` cannot be joined back.
    if m.fs_caps.is_some_and(|c| c.invalid_utf8) {
        anyhow::bail!("{}: the tree holds names that are not UTF-8", m.profile);
    }
    let found: HashMap<&str, (RiskClass, bool)> =
        m.findings.iter().map(|f| (f.path.as_str(), (f.class, f.incomplete))).collect();
    // Owners do not depend on the home.
    let catalog = Catalog::builtin(Path::new("/"));
    let mut items = Vec::new();
    for p in m.plan.iter().filter(|p| !p.path.is_empty()) {
        let action = match p.action.as_str() {
            "trash" => ActionKind::Trash,
            "delete" => ActionKind::Delete,
            _ => continue,
        };
        let (class, incomplete) =
            *found.get(p.path.as_str()).ok_or_else(|| anyhow::anyhow!("{}: not a finding", p.path))?;
        let rule = catalog.by_id(&p.rule).ok_or_else(|| anyhow::anyhow!("{}: no rule {}", p.path, p.rule))?;
        if incomplete || !rule.owners.is_empty() {
            continue;
        }
        let path = tree.join(&p.path);
        let (dev, ino) = ddc_core::platform::meta::file_id(&fs::symlink_metadata(&path)?)?;
        items.push(ListedItem {
            path,
            dev,
            ino,
            rule: p.rule.clone(),
            class,
            action,
            permanent: class == RiskClass::Review && action == ActionKind::Delete,
        });
    }
    if items.is_empty() {
        return Ok(None);
    }
    let list = ListedItems { schema_version: LISTED_SCHEMA_VERSION, items };
    list.validate()?;
    Ok(Some(list))
}

/// Whether every item's own action runs no tool.
pub fn tool_free(list: &ListedItems) -> bool {
    list.items.iter().all(|i| matches!(i.action, ActionKind::Trash | ActionKind::Delete))
}
