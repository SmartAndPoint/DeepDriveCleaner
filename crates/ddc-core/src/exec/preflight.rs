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
//! A runtime's readiness probe is the executor's only: here its process and
//! its context's metadata file stand in for it.
//!
//! A picked entry of the user's own folders gets the clean's entry checks
//! instead (safety ADR, amendment of 2026-10-20), with its lock derived as
//! the scan derives it. Without a scan the dry run cannot see that a child
//! of Downloads became another rule's finding; the clean skips it then. A
//! coding agent's session entry gets the guard's own session check, its
//! companions and its liveness read as the clean reads them (story
//! "Harness data rules"), and a harness's folder the guard's `agent-live`.

use std::cell::RefCell;
use std::io;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::guard::{Guard, Walked, walk_item};
use super::{CommandRunner, ProcessTable, SkipReason};
use crate::class::{Fallback, Location, ReadyProbe, RiskClass};
use crate::conda;
use crate::env::Env;
use crate::model::{Finding, RemovalKind};
use crate::plan::{Action, ActionKind, ListedItem, PlanItem, action_for};
use crate::platform::PLATFORM;
use crate::platform::fsops::DirFd;
use crate::report::backups;
use crate::rules::entries::{self, BACKUP_RULE, EntryFacts, EntryShape, lock_of};
use crate::rules::{Catalog, KEEP_MARKER, LockFiles, Rule, effective_class, protected_in_home};
use crate::scan::DirReader;
use crate::scan::reader::SF_DATALESS;

/// One line of the dry run's output (JSON Lines, one per item, then the summary).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum PreflightLine {
    Item(PreflightItem),
    Summary(PreflightSummary),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PreflightItem {
    #[serde(serialize_with = "crate::json_path::path")]
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
    /// Processes the in-use check could not read in full, as in the clean
    /// report; absent when it read every one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unreadable_processes: Option<u64>,
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
    preflight_skipping(env, catalog, roots, excludes, &[], listed, ports, out)
}

/// [`preflight`] with the scan's skips (`ScanOptions::skip` and
/// `user_skips`): Claude Code's records under one are never read, and a
/// session's entry is then `liveness-unknown`, as the clean's rescan says.
#[allow(clippy::too_many_arguments)]
pub fn preflight_skipping(
    env: &Env,
    catalog: &Catalog,
    roots: &[PathBuf],
    excludes: &[PathBuf],
    skips: &[PathBuf],
    listed: &[ListedItem],
    ports: &PreflightPorts<'_>,
    out: &mut dyn FnMut(&PreflightLine) -> io::Result<()>,
) -> io::Result<PreflightSummary> {
    // Its walk would refuse too, but only after the folders are opened and
    // the in-use checks run, and it folds walk errors away.
    crate::platform::require_supported().map_err(|u| io::Error::new(io::ErrorKind::Unsupported, u))?;
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
    let guard =
        Guard::new(env, catalog, &roots, Vec::new(), ports.procs).with_skips(skips.to_vec()).with_reader(ports.reader);
    let root_refs: Vec<&Path> = roots.iter().map(PathBuf::as_path).collect();
    let sheet = Sheet {
        catalog,
        roots: &root_refs,
        excludes: &excludes,
        guard: &guard,
        ports,
        env,
        locks: RefCell::new(LockFiles::default()),
    };
    // Picked entries first, as the clean runs them: an item inside one that
    // would go to the Trash would be skipped.
    let entries: Vec<Option<(Option<u64>, Prediction)>> = listed
        .iter()
        .map(|item| (item.class == RiskClass::UserData).then(|| settle(check_entry(item, &sheet))))
        .collect();
    let trashed: Vec<&Path> = listed
        .iter()
        .zip(&entries)
        .filter(|(_, e)| matches!(e, Some((_, Prediction::WouldTrash))))
        .map(|(item, _)| item.path.as_path())
        .collect();
    let withheld = guard.withheld();
    let mut summary =
        PreflightSummary { unreadable_processes: (withheld > 0).then_some(withheld), ..PreflightSummary::default() };
    for (item, entry) in listed.iter().zip(entries) {
        let (bytes, prediction) = match entry {
            Some(predicted) => predicted,
            None if trashed.iter().any(|t| item.path != *t && item.path.starts_with(t)) => {
                (None, Prediction::WouldSkip(SkipReason::InsideAnotherItem))
            }
            None => settle(check(item, &sheet)),
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

/// What every item of one list is checked against. Nothing is removed while
/// the list is checked, so what `locks` remembers stays true for all of it.
struct Sheet<'a> {
    catalog: &'a Catalog,
    roots: &'a [&'a Path],
    excludes: &'a [PathBuf],
    guard: &'a Guard<'a>,
    ports: &'a PreflightPorts<'a>,
    env: &'a Env,
    locks: RefCell<LockFiles>,
}

fn settle(checked: Checked) -> (Option<u64>, Prediction) {
    match checked {
        Ok((bytes, prediction)) => (Some(bytes), prediction),
        Err((bytes, reason)) => (bytes, Prediction::WouldSkip(reason)),
    }
}

fn check(item: &ListedItem, sheet: &Sheet<'_>) -> Checked {
    let Sheet { catalog, roots, excludes, guard, ports, env, locks } = sheet;
    let path = item.path.as_path();
    let skip = |reason| Err((None, reason));
    if excludes.iter().any(|e| path.starts_with(e) || e.starts_with(path)) {
        return skip(SkipReason::Kept);
    }
    let Some(rule) = catalog.by_id(&item.rule) else { return skip(SkipReason::RuleMismatch) };
    // Inside a folder with entries, only the folder itself is a finding: an
    // entry listed under another class than `user-data` is none, as the
    // plan finds no finding there.
    if entries::has_entries(rule.id) && !catalog.verify(rule, path) {
        return skip(SkipReason::ChangedSinceScan);
    }
    let class = effective_class(rule, path, roots, env.home(), &mut locks.borrow_mut());
    if class != item.class {
        return skip(SkipReason::ChangedSinceScan);
    }
    let Some(action) = action_for(rule, class, item.permanent) else { return skip(SkipReason::ActionNotAllowed) };
    if action.kind() != item.action {
        return skip(SkipReason::ChangedSinceScan);
    }
    // Identity, symlinks, rule marker, project root, `.ddc-keep` above, owner
    // and use: all before the walk, so nothing behind a swapped path is read.
    let action = action_for(rule, class, item.permanent).unwrap_or(Action::Trash);
    guard.check(&planned(item, rule, class, action)).map_err(|reason| (None, reason))?;
    let walked = walk_item(path, ports.reader).ok_or((None, SkipReason::Incomplete))?;
    if walked.kept {
        return Err((Some(walked.bytes), SkipReason::Kept));
    }
    if walked.incomplete {
        return Err((Some(walked.bytes), SkipReason::Incomplete));
    }
    let prediction = match action {
        Action::Delete { .. } => Prediction::WouldDelete,
        Action::Trash | Action::TrashEntry => Prediction::WouldTrash,
        Action::Native { cmd, fallback } => {
            // conda is never run here: a candidate install's `bin/conda`
            // existing predicts it, as every tool is predicted by its presence.
            let present = match cmd.location {
                Some(Location::Conda(_)) => {
                    conda::spelled(path).is_some()
                        && conda::candidates(env.home(), env.system_root())
                            .iter()
                            .any(|prefix| ports.commands.at(&conda::exe(prefix)).is_some())
                }
                _ => ports.commands.find(cmd.program).is_some(),
            };
            match (present, fallback) {
                (true, _) => Prediction::WouldRun,
                (false, Fallback::Delete | Fallback::DeleteWritable) => Prediction::WouldDelete,
                (false, Fallback::Trash) => Prediction::WouldTrash,
                (false, Fallback::Skip) => {
                    return Err((
                        Some(walked.bytes),
                        SkipReason::ToolNotReady {
                            program: cmd.program.to_owned(),
                            detail: format!("no {} lists this folder", cmd.program),
                        },
                    ));
                }
            }
        }
        Action::NativeOnly(cmds) => match cmds.iter().find(|c| ports.commands.find(c.program).is_none()) {
            Some(missing) => {
                return Err((
                    Some(walked.bytes),
                    SkipReason::NativeUnavailable { program: missing.program.to_owned() },
                ));
            }
            None => match cmds.first().and_then(|c| c.ready).and_then(|probe| not_ready(&probe, guard, env)) {
                Some(reason) => return Err((Some(walked.bytes), reason)),
                None => Prediction::WouldRun,
            },
        },
    };
    Ok((walked.bytes, prediction))
}

/// What the executor's readiness probe would say, from what can be read
/// without running it: the runtime's process, then its context's metadata.
fn not_ready(probe: &ReadyProbe, guard: &Guard<'_>, env: &Env) -> Option<SkipReason> {
    let detail = if !guard.running(probe.process) {
        probe.down
    } else if !env.docker_config().join(probe.context_meta).is_file() {
        probe.no_context
    } else {
        return None;
    };
    Some(SkipReason::ToolNotReady { program: probe.program.to_owned(), detail: detail.to_owned() })
}

/// A picked entry, in the clean's order: the action, the folder itself and
/// its own protection, the path, the folder, the entry and its identity, its
/// lock, then the rest of §5 (the guard).
fn check_entry(item: &ListedItem, sheet: &Sheet<'_>) -> Checked {
    let Sheet { catalog, roots, excludes, guard, ports, env, .. } = sheet;
    let path = item.path.as_path();
    let skip = |reason| Err((None, reason));
    let Some(rule) = catalog.by_id(&item.rule) else { return skip(SkipReason::RuleMismatch) };
    if item.action != ActionKind::Trash || item.permanent {
        return skip(SkipReason::ActionNotAllowed);
    }
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return skip(SkipReason::ActionNotAllowed);
    };
    if catalog.verify(rule, path) {
        return skip(SkipReason::ActionNotAllowed);
    }
    // The whole folder is kept: an exclude at or above it, in a folder the
    // scan claims whole an exclude anywhere inside, or a marker in it or above.
    let claimed_whole = rule.id != "user.downloads";
    if excludes.iter().any(|e| parent.starts_with(e) || (claimed_whole && e.starts_with(parent)))
        || guard.marker_above(path)
    {
        return skip(SkipReason::Kept);
    }
    if !roots.iter().any(|r| path.starts_with(r)) {
        return skip(SkipReason::OutsideScanRoots);
    }
    if parent.canonicalize().ok().as_deref() != Some(parent) {
        return skip(SkipReason::ParentChanged);
    }
    let Some(policy) = entries::policy(rule.id).filter(|_| catalog.verify(rule, parent)) else {
        return skip(SkipReason::NotAnEntry);
    };
    let Ok(parent_fd) = DirFd::open(parent) else { return skip(SkipReason::ParentChanged) };
    let st = match parent_fd.stat_child(name) {
        Ok(st) => st,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return skip(SkipReason::NotAnEntry),
        Err(_) => return skip(SkipReason::ChangedSinceScan),
    };
    if st.is_symlink {
        return skip(SkipReason::Symlink);
    }
    if !(st.is_dir || st.is_file) || st.flags & SF_DATALESS != 0 {
        return skip(SkipReason::NotAnEntry);
    }
    if (st.dev, st.ino) != (item.dev, item.ino) {
        return skip(SkipReason::ChangedSinceScan);
    }
    // A session's entry: the guard's own check finds its companions, walks
    // them with it and reads its liveness, as the clean does.
    if policy.shape != EntryShape::Child {
        let verified = guard
            .check(&planned(item, rule, RiskClass::UserData, Action::TrashEntry))
            .map_err(|reason| (None, reason))?;
        let parts = verified.session.unwrap_or_default();
        let bytes = parts.key_bytes + parts.companions.iter().map(|c| c.bytes).sum::<u64>();
        return Ok((bytes, Prediction::WouldTrash));
    }
    // A volume mounted at the entry itself: locked, and never walked.
    let mounted = parent_fd.identity().is_ok_and(|(dev, _)| dev != st.dev);
    let walked = if mounted {
        Walked { bytes: 0, kept: false, incomplete: true, other_fs: true }
    } else if st.is_dir {
        walk_item(path, ports.reader).unwrap_or(Walked {
            bytes: st.alloc,
            kept: false,
            incomplete: true,
            other_fs: false,
        })
    } else {
        Walked { bytes: st.alloc, kept: false, incomplete: false, other_fs: false }
    };
    let (modified, added) = parent_fd.child_times(name).unwrap_or((None, None));
    let home = env.home();
    let facts = EntryFacts {
        home,
        rule: rule.id,
        path,
        modified,
        added,
        now: env.now(),
        kept: walked.kept
            || excludes.iter().any(|e| e.starts_with(path))
            || (st.is_dir && std::fs::symlink_metadata(path.join(KEEP_MARKER)).is_ok()),
        protected: protected_in_home(PLATFORM).iter().any(|rel| home.join(rel).starts_with(path)),
        other_volume: walked.other_fs,
        // A session's entry took the guard's check above.
        session: None,
        finished: rule.id == BACKUP_RULE && st.is_dir && backups::finished(path),
    };
    if let Some(lock) = lock_of(&facts) {
        return Err((Some(walked.bytes), SkipReason::EntryLocked { lock }));
    }
    guard
        .check(&planned(item, rule, RiskClass::UserData, Action::TrashEntry))
        .map_err(|reason| (Some(walked.bytes), reason))?;
    Ok((walked.bytes, Prediction::WouldTrash))
}

/// The plan item the guard checks: the listed identity under its rule.
fn planned(item: &ListedItem, rule: &Rule, class: RiskClass, action: Action) -> PlanItem {
    let finding = Finding {
        path: item.path.clone(),
        rule: item.rule.clone(),
        category: rule.category,
        class,
        rule_class: rule.class,
        removal: if action == Action::TrashEntry { RemovalKind::Trash } else { RemovalKind::Delete },
        bytes: 0,
        shared_bytes: 0,
        private_bytes: None,
        files: 0,
        dirs: 0,
        incomplete: false,
        project: None,
        selected: true,
        not_selected: None,
        ignored: false,
        notes: Vec::new(),
        hint: String::new(),
        dev: item.dev,
        ino: item.ino,
        parent_dev: None,
        parent_ino: None,
        contents: None,
        protection: None,
        modified: None,
        origin: rule.origin,
    };
    PlanItem { finding, action, default_selected: true, selected: true }
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
