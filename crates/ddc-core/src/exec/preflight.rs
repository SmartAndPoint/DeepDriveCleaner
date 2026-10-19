//! `clean --items --dry-run`: the confirmation sheet's warnings without a
//! rescan (ADR "Menu bar app: Swift shell over the ddc CLI", §6 item 2).
//!
//! Each listed item gets the executor's re-checks on its own: identity, rule,
//! class and action derived again, an exclude, `.ddc-keep`, project roots,
//! owner running, in use. Only the listed items are walked, one at a time.
//! It predicts and changes nothing: there is no Trash and no journal here,
//! and native tools are only looked up, never run. The clean itself still
//! rescans and re-checks; pins only a whole-tree walk sees may skip an item
//! this predicted, and a tool reporting another location runs the fallback.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::guard::Guard;
use super::{CommandRunner, ProcessTable, SkipReason};
use crate::class::{Fallback, RiskClass};
use crate::env::Env;
use crate::model::{Finding, RemovalKind};
use crate::plan::{Action, ActionKind, ListedItem, PlanItem, action_for};
use crate::rules::{Catalog, KEEP_MARKER, effective_class};
use crate::scan::{self, DirCtx, DirReader, EntryMeta, OwnerKind, Progress, Tag, Visitor, WalkOptions};

/// One line of the dry run's output (JSON Lines, one per item, then the summary).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum PreflightLine {
    Item(PreflightItem),
    Summary(PreflightSummary),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PreflightItem {
    pub path: PathBuf,
    pub rule: String,
    pub class: RiskClass,
    pub action: ActionKind,
    /// Allocated bytes now; absent when the item was not walked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(flatten)]
    pub prediction: Prediction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "prediction", rename_all = "kebab-case")]
pub enum Prediction {
    WouldDelete,
    WouldTrash,
    /// The tool's command (installed now; the clean checks its location).
    WouldRun,
    WouldSkip(SkipReason),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PreflightSummary {
    pub items: u64,
    pub would_delete: u64,
    pub would_trash: u64,
    pub would_run: u64,
    pub would_skip: u64,
    /// Bytes of the items not predicted as skipped.
    pub bytes: u64,
}

/// Read-only ports: tools are looked up, never run.
pub struct PreflightPorts<'a> {
    pub commands: &'a dyn CommandRunner,
    pub procs: &'a dyn ProcessTable,
    pub reader: &'a dyn DirReader,
}

/// Checks every listed item and hands `out` one line per item as its checks
/// finish, then the summary line. `roots` and `excludes` are those of the
/// clean that will follow (default: the home directory).
pub fn preflight(
    env: &Env,
    catalog: &Catalog,
    roots: &[PathBuf],
    excludes: &[PathBuf],
    listed: &[ListedItem],
    ports: &PreflightPorts<'_>,
    out: &mut dyn FnMut(&PreflightLine) -> io::Result<()>,
) -> io::Result<PreflightSummary> {
    let roots = canonical_roots(env, roots);
    // As the clean's rescan: an exclude that cannot be resolved is an error, not dropped.
    let excludes = excludes
        .iter()
        .map(|e| {
            e.canonicalize()
                .map_err(|err| io::Error::new(err.kind(), format!("cannot resolve --exclude {}: {err}", e.display())))
        })
        .collect::<io::Result<Vec<PathBuf>>>()?;
    // One process snapshot for the whole sheet; the guard's own keep check
    // covers `.ddc-keep` above an item, the walk below the one inside it.
    let guard = Guard::new(env, catalog, &roots, Vec::new(), ports.procs.processes());
    let root_refs: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    let mut summary = PreflightSummary::default();
    for item in listed {
        let (bytes, prediction) = match check(item, catalog, &root_refs, &excludes, &guard, ports, env.home()) {
            Ok((bytes, prediction)) => (Some(bytes), prediction),
            Err((bytes, reason)) => (bytes, Prediction::WouldSkip(reason)),
        };
        summary.items += 1;
        match &prediction {
            Prediction::WouldDelete => summary.would_delete += 1,
            Prediction::WouldTrash => summary.would_trash += 1,
            Prediction::WouldRun => summary.would_run += 1,
            Prediction::WouldSkip(_) => summary.would_skip += 1,
        }
        if !matches!(prediction, Prediction::WouldSkip(_)) {
            summary.bytes += bytes.unwrap_or(0);
        }
        out(&PreflightLine::Item(PreflightItem {
            path: item.path.clone(),
            rule: item.rule.clone(),
            class: item.class,
            action: item.action,
            bytes,
            prediction,
        }))?;
    }
    out(&PreflightLine::Summary(summary.clone()))?;
    Ok(summary)
}

type Checked = Result<(u64, Prediction), (Option<u64>, SkipReason)>;

fn check(
    item: &ListedItem,
    catalog: &Catalog,
    roots: &[&Path],
    excludes: &[PathBuf],
    guard: &Guard<'_>,
    ports: &PreflightPorts<'_>,
    home: &Path,
) -> Checked {
    let path = item.path.as_path();
    let skip = |reason| Err((None, reason));
    if excludes.iter().any(|e| path.starts_with(e) || e.starts_with(path)) {
        return skip(SkipReason::Kept);
    }
    let Some(rule) = catalog.by_id(&item.rule) else { return skip(SkipReason::RuleMismatch) };
    let class = effective_class(rule, path, roots, home);
    if class != item.class {
        return skip(SkipReason::ChangedSinceScan);
    }
    let Some(action) = action_for(rule, class, item.permanent) else { return skip(SkipReason::ActionNotAllowed) };
    if action.kind() != item.action {
        return skip(SkipReason::ChangedSinceScan);
    }
    // Identity, symlinks, rule marker, project root, `.ddc-keep` above, owner
    // and use: all before the walk, so nothing behind a swapped path is read.
    guard.check(&planned(item, rule, class)).map_err(|reason| (None, reason))?;
    let walked = walk_item(path, ports.reader).ok_or((None, SkipReason::Incomplete))?;
    if walked.kept {
        return Err((Some(walked.bytes), SkipReason::Kept));
    }
    if walked.incomplete {
        return Err((Some(walked.bytes), SkipReason::Incomplete));
    }
    let prediction = match action {
        Action::Delete { .. } => Prediction::WouldDelete,
        Action::Trash => Prediction::WouldTrash,
        Action::Native { cmd, fallback } => match ports.commands.find(cmd.program) {
            Some(_) => Prediction::WouldRun,
            None => match fallback {
                Fallback::Delete | Fallback::DeleteWritable => Prediction::WouldDelete,
                Fallback::Trash => Prediction::WouldTrash,
            },
        },
        Action::NativeOnly(cmds) => match cmds.iter().find(|c| ports.commands.find(c.program).is_none()) {
            Some(missing) => {
                return Err((
                    Some(walked.bytes),
                    SkipReason::NativeUnavailable { program: missing.program.to_owned() },
                ));
            }
            None => Prediction::WouldRun,
        },
    };
    Ok((walked.bytes, prediction))
}

/// The plan item the guard checks: the listed identity under its rule.
fn planned(item: &ListedItem, rule: &crate::rules::Rule, class: RiskClass) -> PlanItem {
    let finding = Finding {
        path: item.path.clone(),
        rule: item.rule.clone(),
        category: rule.category,
        class,
        rule_class: rule.class,
        removal: RemovalKind::Delete,
        bytes: 0,
        shared_bytes: 0,
        private_bytes: None,
        files: 0,
        dirs: 0,
        incomplete: false,
        project: None,
        selected: true,
        not_selected: None,
        notes: Vec::new(),
        hint: String::new(),
        dev: item.dev,
        ino: item.ino,
        parent_dev: None,
        parent_ino: None,
    };
    let action = action_for(rule, class, item.permanent).unwrap_or(Action::Trash);
    PlanItem { finding, action, default_selected: true, selected: true }
}

struct Walked {
    bytes: u64,
    kept: bool,
    incomplete: bool,
}

/// Walks one listed item alone: its size, a `.ddc-keep` inside, unreadable parts.
fn walk_item(path: &Path, reader: &dyn DirReader) -> Option<Walked> {
    struct Markers(Mutex<bool>);
    impl Visitor for Markers {
        fn wants_private(&self, _: &DirCtx<'_>, _: Tag) -> bool {
            false
        }
        fn on_file(&self, _: &Path, entry: &EntryMeta, _: Option<(OwnerKind, Tag)>) {
            if entry.name == KEEP_MARKER {
                *self.0.lock().unwrap_or_else(|p| p.into_inner()) = true;
            }
        }
    }
    let markers = Markers(Mutex::new(false));
    let opts = WalkOptions { threads: Some(2), ..WalkOptions::default() };
    let out = scan::walk_with_reader(
        std::slice::from_ref(&path.to_path_buf()),
        &markers,
        &opts,
        &Progress::default(),
        reader,
    )
    .ok()?;
    let kept = markers.0.into_inner().unwrap_or_else(|p| p.into_inner());
    Some(Walked {
        bytes: out.total_bytes,
        kept,
        incomplete: out.errors_total > 0 || out.skipped_total > 0 || out.vanished > 0,
    })
}

/// The scan's roots: the home directory by default, canonical, nested ones dropped.
fn canonical_roots(env: &Env, roots: &[PathBuf]) -> Vec<PathBuf> {
    let given = if roots.is_empty() { vec![env.home().to_path_buf()] } else { roots.to_vec() };
    let mut canon: Vec<PathBuf> = given.iter().filter_map(|r| r.canonicalize().ok()).collect();
    canon.sort();
    canon.dedup();
    let mut kept: Vec<PathBuf> = Vec::new();
    for r in canon {
        if !kept.iter().any(|k| r.starts_with(k)) {
            kept.push(r);
        }
    }
    kept
}
