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

use std::cell::Cell;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use journal::{
    AreaActionEntry, AreaJournal, FileJournal, JOURNAL_SCHEMA_VERSION, Journal, JournalEntry, JournalKind, KeepJournal,
    KeepRemoveEntry, MemoryAreaJournal, MemoryJournal, MemoryKeepJournal, TrashedTo,
};
pub use native::CommandResult;
pub(crate) use native::tail;
pub use progress::{CurrentItem, ExecPhase, ExecProgress, ExecSnapshot};
pub use system::{SystemCommands, SystemProcesses, SystemTrash};

use crate::agents::HarnessId;
use crate::class::{CondaFolder, Fallback, Location, Removal, RiskClass};
use crate::env::Env;
use crate::plan::{Action, Plan, PlanItem};
use crate::platform::fsops::DirFd;
/// What a [`ProcessTable`] lists: a fake table names it here, not under `platform`.
pub use crate::platform::procs::{OpenVnode, ProcInfo, ProcStart};
/// What a [`Trasher`] answers: a fake Trash names it here, not under `platform`.
pub use crate::platform::types::{TrashError, TrashPlace};
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
    /// Another process holds the folder (Windows only, safety ADR §5,
    /// amendment of 2026-11-02): a process works inside it, or a file below
    /// it is open, so the tombstone rename was refused. Nothing moved.
    HeldOpen,
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
    /// Something inside, or the folder itself, belongs to another user
    /// (safety ADR, amendment of 2026-10-31): refused before any rename.
    ForeignOwner,
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
    /// No Trash on the item's own filesystem that ddc may use, or the move
    /// would cross into another (safety ADR §5, amendment of 2026-11-01):
    /// nothing moved. `--permanent` removes it instead.
    TrashCrossesFilesystem,
    /// A coding agent's harness uses its data: the whole folder is refused
    /// while it runs (ADR "Agent footprint: worktrees, harness data and
    /// bounded consent for agents", §3).
    AgentLive {
        harness: HarnessId,
        by: AgentLiveBy,
    },
    /// A linked worktree that one of the six conditions of ADR "Agent
    /// footprint", §2, holds back, re-checked right before `git worktree
    /// remove` (decision 8 of story "Worktree findings and provenance"):
    /// `condition` is its number, 1 to 6, `detail` says why in words.
    WorktreeKept {
        condition: u8,
        detail: String,
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
    /// Processes the in-use check could not read in full.
    pub withheld_processes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VolumeSpace {
    #[serde(serialize_with = "crate::json_path::path")]
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
    /// [`run`](Self::run), with every variable ddc inherited whose name
    /// starts with `cleared` removed before `env` is added: a git probe
    /// started from inside a git hook must not see the hook's `GIT_DIR` or
    /// `GIT_INDEX_FILE` (story "Worktree findings and provenance").
    fn run_cleared(
        &self,
        exe: &Path,
        args: &[String],
        cleared: &str,
        env: &[(&str, &str)],
        timeout: Duration,
    ) -> io::Result<CmdOutput>;
}

/// What a move to the Trash moves: `name` in the verified folder `parent`
/// (at `path`), with the identity the guard checked.
pub struct TrashTarget<'a> {
    pub path: &'a Path,
    pub parent: &'a DirFd,
    pub name: &'a OsStr,
    pub id: (u64, u64),
    pub is_dir: bool,
}

pub trait Trasher {
    /// Moves `target` to the Trash, and tells where it went when it can.
    fn trash(&self, target: &TrashTarget<'_>) -> Result<Option<TrashPlace>, TrashError>;

    /// The dry run's question: would [`Self::trash`] find a Trash for
    /// `target`? Moves nothing. A Trash that always has one says yes.
    fn would_trash(&self, _target: &TrashTarget<'_>) -> Result<(), TrashError> {
        Ok(())
    }
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
    /// What the machine tells beyond the user's processes, for a coding
    /// agent's old builds (`agents::builds`): read only when a build is found.
    fn host(&self, home: &Path) -> crate::agents::builds::Host {
        crate::agents::builds::Host::read(home)
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
    #[error("refusing to clean as root (LocalSystem on Windows)")]
    Root,
    #[error("--yes cannot include the review class: review items need an interactive confirmation")]
    ReviewWithYes,
    #[error("cannot write the action journal: {0}")]
    Journal(#[from] io::Error),
    /// The platform is not supported: nothing was touched (ADR "Platform
    /// seam: one platform module per OS with declared capabilities", §5).
    #[error("{0}")]
    Unsupported(crate::platform::Unsupported),
    /// A root of the plan another account owns, in an elevated session:
    /// nothing was touched.
    #[error("{0}")]
    ForeignRoot(crate::platform::ForeignRoot),
}

pub struct Ports<'a> {
    pub commands: &'a dyn CommandRunner,
    pub trash: &'a dyn Trasher,
    pub procs: &'a dyn ProcessTable,
    pub journal: &'a mut dyn Journal,
}

/// Native cleanup commands may take long (Go module cache); then they are killed.
pub const NATIVE_TIMEOUT: Duration = Duration::from_secs(600);

/// A question to a tool (a location probe, conda's settings, a git probe)
/// that does not answer within this has failed.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(30);

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
    /// `--worktree-min-age`: a harness's worktree removed under `--yes`
    /// must still be idle this long (condition 3).
    pub worktree_min_age: Duration,
    /// Each git probe's timeout in a worktree's re-check.
    pub probe_timeout: Duration,
}

static NEVER: AtomicBool = AtomicBool::new(false);

impl Default for ExecOptions<'_> {
    fn default() -> Self {
        Self {
            cancel: &NEVER,
            progress: None,
            local_snapshots: None,
            events: None,
            skips: &[],
            worktree_min_age: crate::report::DEFAULT_WORKTREE_MIN_AGE,
            probe_timeout: PROBE_TIMEOUT,
        }
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
    for root in &plan.roots {
        crate::platform::require_own_root(root).map_err(ExecError::ForeignRoot)?;
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
    report.withheld_processes = guard.withheld();
    // Each conda install is asked for its folders once per clean.
    let installs = crate::conda::Installs::new(ports.commands, crate::conda::candidates(env.home(), env.system_root()));
    // Entries that went to the Trash, and worktrees git removed: what lies
    // inside them went with them.
    let mut removed: Vec<PathBuf> = Vec::new();
    let worktrees = WorktreeRecheck { env, consent, commands: ports.commands, opts };
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
        // Where the item went in the Trash, for its journal line.
        let place: Cell<Option<TrashPlace>> = Cell::new(None);
        let outcome = if opts.cancelled() {
            Outcome::NotRun
        } else {
            progress.item_started(index, item);
            if removed.iter().any(|t| path != t && path.starts_with(t)) {
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
                            let (outcome, moved) = trash_session(&ports, &verified, parts, path, &place);
                            key_bytes = Some(parts.key_bytes);
                            companions = moved;
                            outcome
                        }
                        None => act(item, &verified, &guard, &ports, &installs, &worktrees, progress, &place),
                    },
                }
            }
        };
        if (item.action == Action::TrashEntry && outcome == Outcome::Trashed) || removed_by_git(item, &outcome) {
            removed.push(path.clone());
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
                line.trash = place.take().map(TrashedTo::from);
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
    // A linked worktree goes through git alone, and git's handler serves no
    // other rule: never `delete` or `trash` for one (ADR "Agent footprint", §2).
    let through_git = matches!(item.action, Action::Native { cmd, .. } if cmd.location == Some(Location::GitWorktree));
    if through_git != (rule.id == crate::rules::worktree::WORKTREE_RULE) {
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

/// What a worktree's re-check before `git worktree remove` reads.
struct WorktreeRecheck<'a> {
    env: &'a Env,
    consent: &'a Consent,
    commands: &'a dyn CommandRunner,
    opts: &'a ExecOptions<'a>,
}

impl WorktreeRecheck<'_> {
    /// The six conditions again for `item`, a `git.worktree` finding (ADR
    /// "Agent footprint", §2): 1 and 3 are waived when a person listed or
    /// confirmed it, never under `--yes`. Condition 5's accounting asks a
    /// fresh walk of the worktree for its artifacts, only when git lists
    /// ignored paths.
    fn check(&self, item: &PlanItem) -> Result<crate::rules::worktree::Removable, SkipReason> {
        use crate::rules::worktree;
        let path = item.finding.path.as_path();
        let rc = worktree::Recheck {
            commands: self.commands,
            tool_dirs: self.env.tool_dirs(),
            timeout: self.opts.probe_timeout,
            now: self.env.now(),
            min_age: self.opts.worktree_min_age,
        };
        let person = !matches!(self.consent, Consent::AssumeYes { .. });
        // The main checkout the scan recorded: the worktree's project root.
        let Some(main) = item.finding.project.as_ref().map(|p| p.root.as_path()) else {
            return Err(SkipReason::ChangedSinceScan);
        };
        let owners = || std::borrow::Cow::Owned(crate::report::removable_inside(self.env, path, self.opts.skips));
        worktree::recheck(path, Some(main), person, &rc, &owners)
    }
}

/// A `git.worktree` item git removed: what lay inside it went with it.
fn removed_by_git(item: &PlanItem, outcome: &Outcome) -> bool {
    matches!(item.action, Action::Native { cmd, .. } if cmd.location == Some(Location::GitWorktree))
        && matches!(outcome, Outcome::Native { commands } if !commands.is_empty() && commands.iter().all(|c| c.success))
}

#[allow(clippy::too_many_arguments)] // `place`: where the Trash put it, for the journal
fn act(
    item: &PlanItem,
    v: &guard::Verified,
    guard: &guard::Guard<'_>,
    ports: &Ports<'_>,
    installs: &crate::conda::Installs<'_>,
    worktrees: &WorktreeRecheck<'_>,
    progress: &ExecProgress,
    place: &Cell<Option<TrashPlace>>,
) -> Outcome {
    let opts = worktrees.opts;
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
            Fallback::Trash => trash(ports, v, path, place),
            Fallback::Skip => Outcome::Skipped(SkipReason::ToolNotReady {
                program: program.to_owned(),
                detail: format!("no {program} lists this folder"),
            }),
        }
    };
    match item.action {
        Action::Delete { make_writable } => delete::remove(v, path, make_writable),
        Action::Trash | Action::TrashEntry => trash(ports, v, path, place),
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
            let ran = if cmd.location == Some(Location::GitWorktree) {
                match worktrees.check(item) {
                    Ok(removable) => native::run_worktree(&removable, &item.finding, ports.commands, opts),
                    Err(reason) => native::Native::Skip(reason),
                }
            } else {
                native::run(&cmd, path, ports.commands, installs, opts)
            };
            progress.tool(None);
            match ran {
                // git exits 0 once the folder and its administrative folder are gone.
                native::Native::Ran(result)
                    if result.success
                        && cmd.location == Some(Location::GitWorktree)
                        && std::fs::symlink_metadata(path).is_ok() =>
                {
                    Outcome::Failed { error: "git exited 0 and the worktree is still there".to_owned() }
                }
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
    /// Where it went, when the Trash tells.
    pub place: Option<TrashPlace>,
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
    place: &Cell<Option<TrashPlace>>,
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
        let target = TrashTarget { path: &at, parent: &v.parent, name: &c.name, id: c.id, is_dir: c.is_dir };
        let to = match ports.trash.trash(&target) {
            Ok(to) => to,
            Err(TrashError::CrossesFilesystem) if moved.is_empty() => {
                return (Outcome::Skipped(SkipReason::TrashCrossesFilesystem), moved);
            }
            Err(e) => return (Outcome::Failed { error: format!("{}: {e}", at.display()) }, moved),
        };
        moved.push(MovedCompanion { path: at, id: c.id, bytes: c.bytes, place: to });
    }
    match trash(ports, v, path, place) {
        // The key had no Trash after its companions went: they stay there.
        Outcome::Skipped(SkipReason::TrashCrossesFilesystem) if !moved.is_empty() => {
            let error = format!("{}: no Trash on its own filesystem after its companions moved", path.display());
            (Outcome::Failed { error }, moved)
        }
        // The key changed after its companions went: they stay in the Trash.
        Outcome::Skipped(_) if !moved.is_empty() => {
            let error = format!("{} changed after the check", path.display());
            (Outcome::Failed { error }, moved)
        }
        outcome => (outcome, moved),
    }
}

/// `place` is told where the item went, when the Trash tells.
fn trash(ports: &Ports<'_>, v: &guard::Verified, path: &Path, place: &Cell<Option<TrashPlace>>) -> Outcome {
    if !v.unchanged() {
        return Outcome::Skipped(SkipReason::ChangedSinceScan);
    }
    let target = TrashTarget { path, parent: &v.parent, name: &v.name, id: v.id, is_dir: v.is_dir };
    match ports.trash.trash(&target) {
        Ok(to) => {
            place.set(to);
            Outcome::Trashed
        }
        Err(TrashError::CrossesFilesystem) => Outcome::Skipped(SkipReason::TrashCrossesFilesystem),
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
