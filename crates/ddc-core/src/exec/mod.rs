//! Executor: re-checks every plan item, then cleans it (ADR "Cleanup safety
//! model: risk classes, plan and confirmation", §1, §5–§7).
//! Every side effect goes through a port so tests can substitute fakes.

mod delete;
pub mod guard;
mod journal;
mod native;
pub mod system;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;

pub use journal::{FileJournal, Journal, JournalEntry, MemoryJournal};
pub use native::CommandResult;
pub use system::{SystemCommands, SystemProcesses, SystemTrash};

use crate::class::{Fallback, Removal, RiskClass};
use crate::env::Env;
use crate::plan::{Action, Plan, PlanItem};
use crate::rules::{Catalog, KEEP_MARKER};
use crate::scan::{self, EntryMeta, OwnerKind, Progress, Tag, Visitor, WalkOptions};
use crate::sys::procs::ProcInfo;

/// Why an item was left alone. Serialized into the journal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum Outcome {
    Deleted,
    Trashed,
    Native { commands: Vec<CommandResult> },
    Skipped(SkipReason),
    Failed { error: String },
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
    /// Bytes available on the home volume before and after (APFS frees asynchronously).
    pub free_before: Option<u64>,
    pub free_after: Option<u64>,
    /// Local Time Machine snapshots hold deleted blocks until the system thins them.
    pub local_snapshots: usize,
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
    /// Runs without a shell, stdin closed; kills it after `timeout`.
    fn run(&self, exe: &Path, args: &[String], timeout: Duration) -> io::Result<CmdOutput>;
}

pub trait Trasher {
    fn trash(&self, path: &Path) -> io::Result<()>;
}

pub trait ProcessTable {
    fn processes(&self) -> Vec<ProcInfo>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Consent {
    /// The user confirmed each selected item in a terminal.
    Interactive,
    /// `--yes`: only default-selected items of these classes; never `review`
    /// or `native-only`.
    AssumeYes { classes: Vec<RiskClass> },
}

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("refusing to clean as root")]
    Root,
    #[error("--yes cannot include the review class: review items need an interactive confirmation")]
    ReviewWithYes,
    #[error("cannot write the action journal: {0}")]
    Journal(#[from] io::Error),
}

pub struct Ports<'a> {
    pub commands: &'a dyn CommandRunner,
    pub trash: &'a dyn Trasher,
    pub procs: &'a dyn ProcessTable,
    pub journal: &'a mut dyn Journal,
}

/// Native cleanup commands may take long (Go module cache); then they are killed.
pub const NATIVE_TIMEOUT: Duration = Duration::from_secs(600);

pub fn execute(
    env: &Env,
    catalog: &Catalog,
    plan: &Plan,
    consent: &Consent,
    ports: Ports<'_>,
) -> Result<ExecReport, ExecError> {
    if crate::sys::procs::is_root() {
        return Err(ExecError::Root);
    }
    if let Consent::AssumeYes { classes } = consent
        && classes.contains(&RiskClass::Review)
    {
        return Err(ExecError::ReviewWithYes);
    }
    let chosen: Vec<&PlanItem> = plan.items.iter().filter(|i| i.selected).collect();
    let mut report = ExecReport {
        local_snapshots: local_snapshots(ports.commands),
        free_before: free_bytes(env),
        ..ExecReport::default()
    };
    let markers = keep_markers(chosen.iter().map(|i| i.finding.path.clone()).collect());
    let guard = guard::Guard::new(env, catalog, &plan.roots, markers, ports.procs.processes());

    for item in chosen {
        let outcome = if !allowed(item, consent) {
            Outcome::Skipped(SkipReason::NeedsConfirmation)
        } else if let Err(reason) = action_allowed(item, catalog) {
            Outcome::Skipped(reason)
        } else {
            match guard.check(item) {
                Err(reason) => Outcome::Skipped(reason),
                Ok(verified) => act(item, &verified, &ports),
            }
        };
        ports.journal.record(&JournalEntry::new(item, &outcome))?;
        report.items.push(ItemReport {
            path: item.finding.path.clone(),
            rule: item.finding.rule.clone(),
            class: item.finding.class,
            action: item.action.name(),
            bytes: item.finding.bytes,
            outcome,
        });
    }
    report.free_after = free_bytes(env);
    Ok(report)
}

fn allowed(item: &PlanItem, consent: &Consent) -> bool {
    match consent {
        Consent::Interactive => true,
        Consent::AssumeYes { classes } => {
            item.default_selected
                && classes.contains(&item.finding.class)
                && item.finding.class != RiskClass::Review
                && !matches!(item.action, Action::NativeOnly(_))
        }
    }
}

/// The executor does not trust the plan: the action must be one the rule
/// allows for this finding (ADR §1 — enforced here, not only by `build_plan`).
fn action_allowed(item: &PlanItem, catalog: &Catalog) -> Result<(), SkipReason> {
    let Some(rule) = catalog.by_id(&item.finding.rule) else { return Err(SkipReason::ActionNotAllowed) };
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
        (Removal::NativeOnly(cmds), Action::NativeOnly(c)) => cmds == c,
        _ => false,
    };
    if ok { Ok(()) } else { Err(SkipReason::ActionNotAllowed) }
}

fn act(item: &PlanItem, v: &guard::Verified, ports: &Ports<'_>) -> Outcome {
    let path = item.finding.path.as_path();
    // Anything slow (a probe subprocess) may have run: re-check before acting.
    let fallback = |fb: Fallback| {
        if !v.unchanged() {
            return Outcome::Skipped(SkipReason::ChangedSinceScan);
        }
        match fb {
            Fallback::Delete => delete::remove(v, path, false),
            Fallback::DeleteWritable => delete::remove(v, path, true),
            Fallback::Trash => trash(ports, v, path),
        }
    };
    match item.action {
        Action::Delete { make_writable } => delete::remove(v, path, make_writable),
        Action::Trash => trash(ports, v, path),
        Action::Native { cmd, fallback: fb } => match native::run(&cmd, path, ports.commands) {
            native::Native::Ran(result) => Outcome::Native { commands: vec![result] },
            native::Native::Unavailable | native::Native::LocationMismatch => fallback(fb),
        },
        Action::NativeOnly(cmds) => {
            let mut results = Vec::new();
            for cmd in cmds {
                match native::run(cmd, path, ports.commands) {
                    native::Native::Ran(result) => results.push(result),
                    _ => {
                        return Outcome::Skipped(SkipReason::NativeUnavailable { program: cmd.program.to_owned() });
                    }
                }
            }
            Outcome::Native { commands: results }
        }
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

fn free_bytes(env: &Env) -> Option<u64> {
    crate::sys::statfs::statfs(env.home()).ok().map(|s| s.available_bytes)
}

/// Number of local Time Machine snapshots (they keep deleted blocks).
pub fn local_snapshots(commands: &dyn CommandRunner) -> usize {
    let Some(tmutil) = commands.find("tmutil") else { return 0 };
    commands
        .run(&tmutil, &["listlocalsnapshots".to_owned(), "/".to_owned()], Duration::from_secs(30))
        .map(|out| out.stdout.lines().filter(|l| l.contains("com.apple.TimeMachine")).count())
        .unwrap_or(0)
}

/// Walks everything about to be cleaned, collecting directories that hold a
/// keep marker at any depth. Fails closed: a target whose walk fails (or that
/// is no longer a real directory) is reported as kept itself.
fn keep_markers(targets: Vec<PathBuf>) -> Vec<PathBuf> {
    struct Markers(Mutex<Vec<PathBuf>>);
    impl Visitor for Markers {
        fn wants_private(&self, _: &scan::DirCtx<'_>, _: Tag) -> bool {
            false
        }
        fn on_file(&self, dir: &Path, entry: &EntryMeta, _: Option<(OwnerKind, Tag)>) {
            if entry.name == KEEP_MARKER {
                self.0.lock().unwrap_or_else(|p| p.into_inner()).push(dir.to_path_buf());
            }
        }
    }
    let markers = Markers(Mutex::new(Vec::new()));
    for target in targets {
        if !std::fs::symlink_metadata(&target).is_ok_and(|m| m.is_dir()) {
            continue; // missing or swapped: the guard skips it anyway
        }
        let walked = scan::walk(std::slice::from_ref(&target), &markers, &WalkOptions::default(), &Progress::default());
        if walked.is_err() {
            markers.0.lock().unwrap_or_else(|p| p.into_inner()).push(target);
        }
    }
    markers.0.into_inner().unwrap_or_else(|p| p.into_inner())
}
