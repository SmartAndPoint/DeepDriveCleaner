//! Executor: re-checks every plan item, then cleans it (ADR "Cleanup safety
//! model: risk classes, plan and confirmation", §1, §5–§7).
//! Every side effect goes through a port so tests can substitute fakes.

mod delete;
pub mod guard;
mod journal;
mod native;
pub mod preflight;
mod progress;
pub mod report;
pub mod system;

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use journal::{
    AreaActionEntry, AreaJournal, FileJournal, JOURNAL_SCHEMA_VERSION, Journal, JournalEntry, JournalKind, KeepJournal,
    KeepRemoveEntry, MemoryAreaJournal, MemoryJournal, MemoryKeepJournal,
};
pub use native::CommandResult;
pub(crate) use native::tail;
pub use progress::{CurrentItem, ExecPhase, ExecProgress, ExecSnapshot};
pub use system::{SystemCommands, SystemProcesses, SystemTrash};

use crate::agents::HarnessId;
use crate::class::{CondaFolder, Fallback, Location, Removal, RiskClass};
use crate::env::Env;
use crate::plan::{Action, Plan, PlanItem};
/// What a [`ProcessTable`] lists: a fake table names it here, not under `platform`.
pub use crate::platform::procs::{OpenVnode, ProcInfo, ProcStart};
use crate::rules::entries::{self, EntryLock};
use crate::rules::{Catalog, KEEP_MARKER};
use crate::scan::{self, EntryMeta, OwnerKind, Progress, Tag, Visitor, WalkOptions};

/// Why an item was left alone. Serialized into the journal and the clean report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum SkipReason {
    Missing,
    NotADirectory,
    Symlink,
    /// Same path, different directory than the scan saw.
    ChangedSinceScan,
    /// A symlink or another directory now sits above the finding.
    ParentChanged,
    /// The rule's marker no longer matches.
    RuleMismatch,
    OutsideScanRoots,
    ScanRoot,
    Protected,
    ProjectRoot,
    /// `.ddc-keep` in it or above it.
    Kept,
    InUse {
        pid: i32,
        name: String,
    },
    InUseUnknownCwd {
        pid: i32,
        name: String,
    },
    /// An app that owns the item is running; `name` is its bundle name.
    OwnerRunning {
        pid: i32,
        name: String,
    },
    /// `review` and `native-only` items need an interactive confirmation.
    NeedsConfirmation,
    /// The plan asks for a method the rule does not allow (user data,
    /// report-only, a VM runtime with anything but its prune commands).
    ActionNotAllowed,
    /// Part of it could not be read or another filesystem is mounted inside.
    Incomplete,
    NativeUnavailable {
        program: String,
    },
    /// The runtime did not answer before its prune commands; nothing ran.
    /// `detail` says why: not running, no context, or the tool's last line.
    ToolNotReady {
        program: String,
        detail: String,
    },
    /// A picked user-data item that is no longer an entry of its folder.
    NotAnEntry,
    /// A picked entry that is locked now.
    EntryLocked {
        lock: EntryLock,
    },
    /// Inside an entry of the same list that went to the Trash.
    InsideAnotherItem,
    /// A coding agent's harness uses its data: the whole folder is refused
    /// while it runs (ADR "Agent footprint: worktrees, harness data and
    /// bounded consent for agents", §3).
    AgentLive {
        harness: HarnessId,
        by: AgentLiveBy,
    },
}

/// What shows a harness in use, for [`SkipReason::AgentLive`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AgentLiveBy {
    /// A process of the harness runs.
    Running,
    /// A process of the user holds a Codex thread lock open.
    LockHeld,
    /// It could not be told: a process of the harness ddc cannot name, or
    /// Codex's locks could not be read.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum Outcome {
    Deleted,
    Trashed,
    Native {
        commands: Vec<CommandResult>,
    },
    Skipped(SkipReason),
    Failed {
        error: String,
    },
    /// Left by a cancel before it started; never journaled.
    NotRun,
}

#[derive(Debug, Clone)]
pub struct ItemReport {
    pub path: PathBuf,
    pub rule: String,
    pub class: RiskClass,
    pub action: &'static str,
    pub bytes: u64,
    pub outcome: Outcome,
}

#[derive(Debug, Clone, Default)]
pub struct ExecReport {
    pub items: Vec<ItemReport>,
    /// Free space of each volume an item lives on, before and after (APFS frees asynchronously).
    pub volumes: Vec<VolumeSpace>,
    /// Local Time Machine snapshots hold deleted blocks until the system thins them.
    pub local_snapshots: usize,
    /// A cancel stopped the run; the items it left are `NotRun`.
    pub cancelled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VolumeSpace {
    pub mount_point: PathBuf,
    /// Bytes available to the user.
    pub free_before: u64,
    pub free_after: u64,
}

impl ExecReport {
    pub fn bytes_where(&self, pred: impl Fn(&Outcome) -> bool) -> u64 {
        self.items.iter().filter(|i| pred(&i.outcome)).map(|i| i.bytes).sum()
    }
}

#[derive(Debug, Clone, Default)]
pub struct CmdOutput {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub trait CommandRunner {
    /// Absolute path of `program`, or `None` when not installed.
    fn find(&self, program: &str) -> Option<PathBuf>;
    /// `exe` itself when it is an executable file: a system tool run by its
    /// absolute path, never searched for.
    fn at(&self, exe: &Path) -> Option<PathBuf> {
        system::executable(exe).then(|| exe.to_path_buf())
    }
    /// Runs without a shell, stdin closed, with `env` added to the
    /// environment; kills it after `timeout`.
    fn run(&self, exe: &Path, args: &[String], env: &[(&str, &str)], timeout: Duration) -> io::Result<CmdOutput>;
}

pub trait Trasher {
    fn trash(&self, path: &Path) -> io::Result<()>;
}

pub trait ProcessTable {
    fn processes(&self) -> Vec<ProcInfo>;
    /// The files one process has open now, for the guard's confirming re-read.
    fn open_files(&self, pid: i32) -> Vec<PathBuf>;
    /// When one process started and whose it is; `None` when it is gone or
    /// cannot be read. A coding agent's session record names its process by
    /// pid, and this tells a reused pid apart.
    fn process_start(&self, pid: i32) -> Option<ProcStart>;
    /// The kernel's `(dev, ino)` of each file one process has open now,
    /// matched against a `stat` of a known file, never `stat`ed itself.
    fn open_vnodes(&self, pid: i32) -> Vec<OpenVnode>;
    /// The user's pids alone, for a sweep of their open files that needs
    /// nothing else of them: the system's table reads no other detail.
    fn pids(&self) -> Vec<i32> {
        self.processes().iter().map(|p| p.pid).collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Consent {
    /// The user confirmed each selected item in a terminal.
    Interactive,
    /// `--yes`: only default-selected items of these classes; never `review`
    /// or `native-only`.
    AssumeYes { classes: Vec<RiskClass> },
    /// `--items`: the user confirmed exactly these items in the app's sheet;
    /// interactive consent (ADR "Menu bar app: Swift shell over the ddc CLI", §7).
    Listed,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("refusing to clean as root")]
    Root,
    #[error("--yes cannot include the review class: review items need an interactive confirmation")]
    ReviewWithYes,
    #[error("cannot write the action journal: {0}")]
    Journal(#[from] io::Error),
    /// The platform is not supported: nothing was touched (ADR "Platform
    /// seam: one platform module per OS with declared capabilities", §5).
    #[error("{0}")]
    Unsupported(crate::platform::Unsupported),
}

pub struct Ports<'a> {
    pub commands: &'a dyn CommandRunner,
    pub trash: &'a dyn Trasher,
    pub procs: &'a dyn ProcessTable,
    pub journal: &'a mut dyn Journal,
}

/// Native cleanup commands may take long (Go module cache); then they are killed.
pub const NATIVE_TIMEOUT: Duration = Duration::from_secs(600);

/// A runtime whose readiness probe does not answer within this is not ready.
pub const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// Told about the run as it goes, on the executing thread (story "CLI:
/// progress events"): the progress events that must reach stderr in order.
pub trait ExecEvents: std::fmt::Debug {
    /// Once, after the executor's preparation and immediately before the
    /// first action, even when no item runs; never when `execute` refuses.
    fn cleaning(&self) {}
    /// Once per item, after its journal record (at once for a `not-run`
    /// item, which is never journaled) and after the counter counted it.
    fn item(&self, _i: u64, _n: u64, _item: &PlanItem, _outcome: &Outcome) {}
}

/// How a run is steered from outside.
#[derive(Debug, Clone, Copy)]
pub struct ExecOptions<'a> {
    /// Set by a signal handler (or a test): the run stops before the next item
    /// and before any fallback, and leaves the remaining items `NotRun`.
    pub cancel: &'a AtomicBool,
    /// Live counters for the caller's progress display (story "Progress
    /// output while ddc clean runs").
    pub progress: Option<&'a ExecProgress>,
    /// Local snapshots the caller already counted, so `tmutil` runs once;
    /// `None` counts them here.
    pub local_snapshots: Option<usize>,
    /// Told on the executing thread before the first action and after each
    /// item (story "CLI: progress events").
    pub events: Option<&'a dyn ExecEvents>,
    /// The scan's skips (`ScanOptions::skip` and `user_skips`): the guard
    /// reads no harness's records or locks under one.
    pub skips: &'a [PathBuf],
}

static NEVER: AtomicBool = AtomicBool::new(false);

impl Default for ExecOptions<'_> {
    fn default() -> Self {
        Self { cancel: &NEVER, progress: None, local_snapshots: None, events: None, skips: &[] }
    }
}

impl ExecOptions<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

pub fn execute(
    env: &Env,
    catalog: &Catalog,
    plan: &Plan,
    consent: &Consent,
    ports: Ports<'_>,
    opts: &ExecOptions<'_>,
) -> Result<ExecReport, ExecError> {
    crate::platform::require_supported().map_err(ExecError::Unsupported)?;
    if crate::platform::procs::is_root() {
        return Err(ExecError::Root);
    }
    if let Consent::AssumeYes { classes } = consent
        && classes.contains(&RiskClass::Review)
    {
        return Err(ExecError::ReviewWithYes);
    }
    let chosen: Vec<&PlanItem> = plan.items.iter().filter(|i| i.selected).collect();
    let unshared = ExecProgress::default();
    let progress = opts.progress.unwrap_or(&unshared);
    progress.start(&chosen);
    let volumes = volumes_of(&chosen, env);
    let snapshots = opts.local_snapshots.unwrap_or_else(|| local_snapshots(ports.commands));
    let mut report = ExecReport { local_snapshots: snapshots, ..ExecReport::default() };
    let markers = keep_markers(chosen.iter().map(|i| i.finding.path.clone()).collect(), opts, progress.walk());
    let guard = guard::Guard::new(env, catalog, &plan.roots, markers, ports.procs).with_skips(opts.skips.to_vec());
    // Each conda install is asked for its folders once per clean.
    let installs = crate::conda::Installs::new(ports.commands, crate::conda::candidates(env.home(), env.system_root()));
    // Entries that went to the Trash: what lies inside them went with them.
    let mut trashed: Vec<PathBuf> = Vec::new();
    // Before the phase flips, so no item counter reaches stderr ahead of it.
    if let Some(events) = opts.events {
        events.cleaning();
    }
    progress.begin_cleaning();

    let n = chosen.len() as u64;
    for (index, item) in (1..).zip(chosen) {
        let path = &item.finding.path;
        // A session's entry: its key's own bytes, and the companions moved.
        let mut key_bytes: Option<u64> = None;
        let mut companions: Vec<MovedCompanion> = Vec::new();
        let outcome = if opts.cancelled() {
            Outcome::NotRun
        } else {
            progress.item_started(index, item);
            if trashed.iter().any(|t| path != t && path.starts_with(t)) {
                Outcome::Skipped(SkipReason::InsideAnotherItem)
            } else if !allowed(item, consent) {
                Outcome::Skipped(SkipReason::NeedsConfirmation)
            } else if let Err(reason) = action_allowed(item, catalog, consent) {
                Outcome::Skipped(reason)
            } else {
                match guard.check(item) {
                    Err(reason) => Outcome::Skipped(reason),
                    Ok(verified) => match &verified.session {
                        Some(parts) => {
                            let (outcome, moved) = trash_session(&ports, &verified, parts, path);
                            key_bytes = Some(parts.key_bytes);
                            companions = moved;
                            outcome
                        }
                        None => act(item, &verified, &guard, &ports, &installs, opts, progress),
                    },
                }
            }
        };
        if item.action == Action::TrashEntry && outcome == Outcome::Trashed {
            trashed.push(path.clone());
        }
        if outcome == Outcome::NotRun {
            report.cancelled = true;
        } else {
            // One line per moved path: the companions first, as they moved,
            // then the entry's own line with its key's bytes.
            let mut recorded: io::Result<()> = Ok(());
            for companion in &companions {
                recorded = ports.journal.record(&JournalEntry::companion(item, companion));
                if recorded.is_err() {
                    break;
                }
            }
            if recorded.is_ok() {
                let mut line = JournalEntry::new(item, &outcome);
                if let Some(bytes) = key_bytes {
                    line.bytes = bytes;
                }
                recorded = ports.journal.record(&line);
            }
            if recorded.is_err() {
                progress.item_finished(item, false);
            }
            recorded?;
        }
        progress.item_finished(item, outcome != Outcome::NotRun);
        if let Some(events) = opts.events {
            events.item(index, n, item, &outcome);
        }
        report.items.push(ItemReport {
            path: item.finding.path.clone(),
            rule: item.finding.rule.clone(),
            class: item.finding.class,
            action: item.action.name(),
            bytes: item.finding.bytes,
            outcome,
        });
    }
    report.volumes = volumes
        .into_iter()
        .filter_map(|(mount_point, free_before)| {
            let free_after = crate::platform::statfs::statfs(&mount_point).ok()?.available_bytes;
            Some(VolumeSpace { mount_point, free_before, free_after })
        })
        .collect();
    Ok(report)
}

fn allowed(item: &PlanItem, consent: &Consent) -> bool {
    match consent {
        Consent::Interactive | Consent::Listed => true,
        Consent::AssumeYes { classes } => {
            item.default_selected
                && classes.contains(&item.finding.class)
                && item.finding.class != RiskClass::Review
                && !matches!(item.action, Action::NativeOnly(_))
        }
    }
}

/// The executor does not trust the plan: the action must be one the rule
/// allows for this finding (ADR §1 — enforced here, not only by `build_plan`),
/// under a consent the rule allows.
/// The one way into user data is an entry of the user's own folders that
/// the user listed (safety ADR, amendment of 2026-10-20); a coding agent's
/// session entry, listed, is the other way to the Trash of a folder whose
/// own method is `none` (ADR "Agent footprint", §3).
fn action_allowed(item: &PlanItem, catalog: &Catalog, consent: &Consent) -> Result<(), SkipReason> {
    let Some(rule) = catalog.by_id(&item.finding.rule) else { return Err(SkipReason::ActionNotAllowed) };
    // A `listed_only` rule's item runs from a person's list only, whatever
    // its class: defence in depth behind `build_plan` and `review-with-yes`.
    if rule.listed_only && *consent != Consent::Listed {
        return Err(SkipReason::ActionNotAllowed);
    }
    if item.action == Action::TrashEntry {
        // The user's own folders are `user-data`; a session folder is not.
        let entry = *consent == Consent::Listed
            && entries::policy(rule.id).is_some_and(|p| p.user_data == (rule.class == RiskClass::UserData))
            && rule.removal == Removal::None
            && item.finding.class == RiskClass::UserData;
        return if entry { Ok(()) } else { Err(SkipReason::ActionNotAllowed) };
    }
    if rule.class == RiskClass::UserData || item.finding.class == RiskClass::UserData {
        return Err(SkipReason::ActionNotAllowed);
    }
    let ok = match (rule.removal, item.action) {
        (Removal::Delete, Action::Delete { .. }) => true,
        // A rule downgraded to review goes to the Trash.
        (Removal::Delete, Action::Trash) => true,
        // `--permanent` turns Trash into delete.
        (Removal::Trash, Action::Trash | Action::Delete { .. }) => true,
        (Removal::Native { cmd, fallback }, Action::Native { cmd: c, fallback: f }) => cmd == c && fallback == f,
        // An empty command list runs nothing and must never read as "ran tools".
        (Removal::NativeOnly(cmds), Action::NativeOnly(c)) => cmds == c && !c.is_empty(),
        _ => false,
    };
    if ok { Ok(()) } else { Err(SkipReason::ActionNotAllowed) }
}

fn act(
    item: &PlanItem,
    v: &guard::Verified,
    guard: &guard::Guard<'_>,
    ports: &Ports<'_>,
    installs: &crate::conda::Installs<'_>,
    opts: &ExecOptions<'_>,
    progress: &ExecProgress,
) -> Outcome {
    let path = item.finding.path.as_path();
    // Anything slow (a probe subprocess) may have run: re-check before acting.
    let fallback = |fb: Fallback, program: &str| {
        // A probe killed by a forwarded signal fails like a location mismatch:
        // a cancel must stop here, never turn into the fallback's delete.
        if opts.cancelled() {
            return Outcome::NotRun;
        }
        if !v.unchanged() {
            return Outcome::Skipped(SkipReason::ChangedSinceScan);
        }
        match fb {
            Fallback::Delete => delete::remove(v, path, false),
            Fallback::DeleteWritable => delete::remove(v, path, true),
            Fallback::Trash => trash(ports, v, path),
            Fallback::Skip => Outcome::Skipped(SkipReason::ToolNotReady {
                program: program.to_owned(),
                detail: format!("no {program} lists this folder"),
            }),
        }
    };
    match item.action {
        Action::Delete { make_writable } => delete::remove(v, path, make_writable),
        Action::Trash | Action::TrashEntry => trash(ports, v, path),
        Action::Native { cmd, fallback: fb } => {
            // A long clean reaches this item minutes after the guard's snapshot,
            // and a tool waits on a lock a process took meanwhile (uv).
            if let Some(reason) = guard.before_tool(path) {
                return Outcome::Skipped(reason);
            }
            if opts.cancelled() {
                return Outcome::NotRun;
            }
            progress.tool(Some(cmd.program));
            let ran = native::run(&cmd, path, ports.commands, installs, opts);
            progress.tool(None);
            match ran {
                // conda can exit 0 having removed nothing (an environment it
                // cannot write): an environment still there is not removed.
                native::Native::Ran(result)
                    if result.success
                        && cmd.location == Some(Location::Conda(CondaFolder::Environment))
                        && std::fs::symlink_metadata(path).is_ok() =>
                {
                    Outcome::Failed { error: format!("{} exited 0 and the environment is still there", result.program) }
                }
                native::Native::Ran(result) => Outcome::Native { commands: vec![result] },
                native::Native::Skip(reason) => Outcome::Skipped(reason),
                native::Native::Cancelled => Outcome::NotRun,
                native::Native::Unavailable | native::Native::LocationMismatch | native::Native::NotReady(_) => {
                    fallback(fb, cmd.program)
                }
            }
        }
        Action::NativeOnly(cmds) => {
            // A missing tool stays `native-unavailable`; the probe runs once,
            // and no prune command runs when it fails.
            if let Some(missing) = cmds.iter().find(|c| ports.commands.find(c.program).is_none()) {
                return Outcome::Skipped(SkipReason::NativeUnavailable { program: missing.program.to_owned() });
            }
            if let Some(probe) = cmds.first().and_then(|c| c.ready) {
                progress.tool(Some(probe.program));
                let not_ready = native::ready(&probe, ports.commands, opts);
                progress.tool(None);
                match not_ready {
                    None => {}
                    Some(native::Native::NotReady(detail)) => {
                        return Outcome::Skipped(SkipReason::ToolNotReady {
                            program: probe.program.to_owned(),
                            detail,
                        });
                    }
                    Some(native::Native::Cancelled) => return Outcome::NotRun,
                    Some(_) => {
                        return Outcome::Skipped(SkipReason::NativeUnavailable { program: probe.program.to_owned() });
                    }
                }
            }
            let mut results = Vec::new();
            for cmd in cmds {
                if opts.cancelled() {
                    break;
                }
                progress.tool(Some(cmd.program));
                let ran = native::run(cmd, path, ports.commands, installs, opts);
                progress.tool(None);
                match ran {
                    native::Native::Ran(result) => results.push(result),
                    native::Native::Cancelled => break,
                    native::Native::Skip(reason) => return Outcome::Skipped(reason),
                    native::Native::Unavailable | native::Native::LocationMismatch | native::Native::NotReady(_) => {
                        return Outcome::Skipped(SkipReason::NativeUnavailable { program: cmd.program.to_owned() });
                    }
                }
            }
            if results.is_empty() { Outcome::NotRun } else { Outcome::Native { commands: results } }
        }
    }
}

/// A companion of a session's entry that went to the Trash with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovedCompanion {
    pub path: PathBuf,
    pub id: (u64, u64),
    pub bytes: u64,
}

/// A session's entry: each companion to the Trash first, by name, each
/// checked unchanged through the parent descriptor right before its move,
/// then the key (the transcript) last, so a session whose transcript is
/// still there is never one with nothing left to resume. The first failure
/// stops it: what moved stays moved, and is journaled; the key stays.
fn trash_session(
    ports: &Ports<'_>,
    v: &guard::Verified,
    parts: &guard::SessionParts,
    path: &Path,
) -> (Outcome, Vec<MovedCompanion>) {
    let mut moved = Vec::new();
    let Some(parent) = path.parent() else { return (Outcome::Skipped(SkipReason::Protected), moved) };
    for c in &parts.companions {
        let unchanged = v.parent.stat_child(&c.name).is_ok_and(|st| {
            !st.is_symlink && st.is_dir == c.is_dir && (st.is_dir || st.is_file) && (st.dev, st.ino) == c.id
        });
        let at = parent.join(&c.name);
        if !unchanged {
            let outcome = if moved.is_empty() {
                Outcome::Skipped(SkipReason::ChangedSinceScan)
            } else {
                Outcome::Failed { error: format!("{} changed after the check", at.display()) }
            };
            return (outcome, moved);
        }
        if let Err(e) = ports.trash.trash(&at) {
            return (Outcome::Failed { error: format!("{}: {e}", at.display()) }, moved);
        }
        moved.push(MovedCompanion { path: at, id: c.id, bytes: c.bytes });
    }
    match trash(ports, v, path) {
        // The key changed after its companions went: they stay in the Trash.
        Outcome::Skipped(_) if !moved.is_empty() => {
            let error = format!("{} changed after the check", path.display());
            (Outcome::Failed { error }, moved)
        }
        outcome => (outcome, moved),
    }
}

fn trash(ports: &Ports<'_>, v: &guard::Verified, path: &Path) -> Outcome {
    if !v.unchanged() {
        return Outcome::Skipped(SkipReason::ChangedSinceScan);
    }
    match ports.trash.trash(path) {
        Ok(()) => Outcome::Trashed,
        Err(e) => Outcome::Failed { error: e.to_string() },
    }
}

/// The volumes the items live on, with their free space now (the home
/// volume when nothing is chosen).
fn volumes_of(chosen: &[&PlanItem], env: &Env) -> BTreeMap<PathBuf, u64> {
    let mut paths: Vec<&Path> = chosen.iter().map(|i| i.finding.path.as_path()).collect();
    if paths.is_empty() {
        paths.push(env.home());
    }
    paths
        .into_iter()
        .filter_map(|p| crate::platform::statfs::statfs(p).ok())
        .map(|st| (st.mount_point, st.available_bytes))
        .collect()
}

/// Number of local Time Machine snapshots (they keep deleted blocks).
pub fn local_snapshots(commands: &dyn CommandRunner) -> usize {
    local_snapshots_with(commands, crate::platform::CAPABILITIES)
}

/// [`local_snapshots`] on a platform that declares `capabilities`: 0, and no
/// tool run, without `local-snapshots` (ADR "Platform seam: one platform
/// module per OS with declared capabilities", §3).
pub fn local_snapshots_with(commands: &dyn CommandRunner, capabilities: &[crate::platform::Capability]) -> usize {
    if !capabilities.contains(&crate::platform::Capability::LocalSnapshots) {
        return 0;
    }
    commands.find("tmutil").and_then(|tmutil| count_local_snapshots(commands, &tmutil)).unwrap_or(0)
}

/// Local Time Machine snapshots of `/` as `tmutil` at `exe` lists them;
/// `None` when it cannot be run or fails.
pub fn count_local_snapshots(commands: &dyn CommandRunner, exe: &Path) -> Option<usize> {
    let out = commands
        .run(exe, &["listlocalsnapshots".to_owned(), "/".to_owned()], &[], Duration::from_secs(30))
        .ok()
        .filter(|out| out.success)?;
    Some(out.stdout.lines().filter(|l| l.contains("com.apple.TimeMachine")).count())
}

/// Walks everything about to be cleaned, collecting directories that hold a
/// keep marker at any depth. Fails closed: a target whose walk fails or is
/// cancelled midway is reported as kept itself (one no longer a real
/// directory is the guard's to skip). `walked`
/// counts across the targets, for the caller's progress display.
fn keep_markers(targets: Vec<PathBuf>, opts: &ExecOptions<'_>, walked: &Progress) -> Vec<PathBuf> {
    struct Markers(Mutex<Vec<PathBuf>>);
    impl Visitor for Markers {
        fn wants_private(&self, _: &scan::DirCtx<'_>, _: Tag) -> bool {
            false
        }
        fn on_file(&self, dir: &Path, _: (u64, u64), entry: &EntryMeta, _: Option<(OwnerKind, Tag)>) {
            if entry.name == KEEP_MARKER {
                self.0.lock().unwrap_or_else(|p| p.into_inner()).push(dir.to_path_buf());
            }
        }
    }
    let markers = Markers(Mutex::new(Vec::new()));
    for target in targets {
        if opts.cancelled() {
            break; // every item is then `NotRun`
        }
        if !std::fs::symlink_metadata(&target).is_ok_and(|m| m.is_dir()) {
            continue; // missing or swapped: the guard skips it anyway
        }
        let result = scan::walk(std::slice::from_ref(&target), &markers, &WalkOptions::default(), walked);
        if !result.is_ok_and(|out| !out.cancelled) {
            markers.0.lock().unwrap_or_else(|p| p.into_inner()).push(target);
        }
    }
    markers.0.into_inner().unwrap_or_else(|p| p.into_inner())
}
