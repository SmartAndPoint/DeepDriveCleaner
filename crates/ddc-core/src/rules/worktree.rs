//! Linked git worktrees (ADR "Agent footprint: worktrees, harness data and
//! bounded consent for agents", §2 and its amendment of 2026-11-01): what a
//! folder whose `.git` is a file is, read from the files git keeps, never by
//! running git. The rules `git.worktree` and `git.worktree-orphan` make such
//! a folder a region, so the other rules keep working inside it.

use crate::platform::Canonical as _;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::io::{ErrorKind, Read as _};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use rayon::prelude::*;

use super::{Classified, KEPT, Matcher, Rule};
use crate::class::{Removal, RiskClass};
use crate::exec::guard::Users;
use crate::exec::{CommandRunner, ProcessTable, SkipReason};
use crate::git::{self, Git, Refs};
use crate::model::{IgnoredContent, OriginHarness, WorktreeProbe, WorktreeState};
use crate::report::project::{self, git_dir};
use crate::scan::OwnerKind;

/// What a folder whose `.git` file names a gitdir is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recognized {
    /// A linked worktree: its gitdir exists and has a `commondir`.
    Linked(Linked),
    /// Its gitdir, `…/worktrees/<name>`, does not exist: the repository was
    /// removed or moved, or lies on a volume that is not mounted. The folder
    /// is a copy of files, and nothing says whose project it is: the
    /// artifacts inside keep every downgrade.
    Orphan { harness: OriginHarness },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Linked {
    /// Its administrative folder, `<main>/.git/worktrees/<name>` (or
    /// `<bare>/worktrees/<name>`).
    pub gitdir: PathBuf,
    /// The main checkout, from `<gitdir>/commondir`: the folder holding the
    /// common `.git`, or a bare repository itself.
    pub main: PathBuf,
    pub harness: OriginHarness,
    /// `<gitdir>/gitdir` names this folder's `.git` by file identity: false
    /// for a worktree copied or moved by hand (decision 4 of the story's plan).
    pub linked: bool,
}

/// Claude Code's CLI writes the base commit here, 40 hex digits.
const CLAUDE_BASE: &str = "CLAUDE_BASE";
/// Claude Desktop writes this empty file.
const CLAUDE_DESKTOP: &str = "claude-desktop-worktree";
/// Codex writes its thread here.
const CODEX_THREAD: &str = "codex-thread.json";
/// The files read here are a line each; anything longer is not git's.
const MAX_READ: u64 = 4096;

/// What `path` is when its `.git` is a file with a `gitdir:` line: a linked
/// worktree, an orphan, or `None` for anything else (no `.git` file, a
/// gitdir without `commondir` such as a submodule's, an unreadable one).
pub fn recognize(path: &Path) -> Option<Recognized> {
    let dot_git = path.join(".git");
    if !fs::symlink_metadata(&dot_git).ok()?.is_file() {
        return None;
    }
    let named = git_dir(path)?;
    let gitdir = match fs::metadata(&named) {
        // Canonical, checked again: a local symlink may lead to a share
        // (one open reached it; nothing more goes there).
        Ok(md) if md.is_dir() => named.canonical().ok().filter(|g| crate::platform::paths::is_local(g))?,
        Ok(_) => return None,
        // Only a worktree's gitdir, `…/worktrees/<name>`: a submodule copied
        // out of its superproject names a missing `…/modules/<name>` and is
        // none (no finding, no region), as before.
        Err(e)
            if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory)
                && normalize(&named).parent().and_then(Path::file_name).is_some_and(|n| n == "worktrees") =>
        {
            return Some(Recognized::Orphan { harness: orphan_harness(path) });
        }
        Err(_) => return None,
    };
    // A submodule's gitdir (`.git/modules/<n>`) has no `commondir`: never a worktree.
    let common = resolve(&gitdir, read_line(&gitdir.join("commondir"))?.as_str());
    // `commondir` and the `gitdir` back-reference come from files' content:
    // a path on another machine is never followed (`git_dir` checked the
    // `.git` file's own line).
    if !crate::platform::paths::is_local(&common) {
        return None;
    }
    let main = if common.file_name().is_some_and(|n| n == ".git") { common.parent()?.to_path_buf() } else { common };
    let main = main.canonical().unwrap_or(main);
    if !crate::platform::paths::is_local(&main) {
        return None;
    }
    let linked = read_line(&gitdir.join("gitdir"))
        .map(|back| resolve(&gitdir, &back))
        .is_some_and(|back| crate::platform::paths::is_local(&back) && same_file(&back, &dot_git));
    let harness = harness(path, &gitdir, &main);
    Some(Recognized::Linked(Linked { gitdir, main, harness, linked }))
}

/// Whether `rule` is one of the two worktree rules.
pub fn is_worktree_rule(rule: &Rule) -> bool {
    matches!(rule.matcher, Matcher::Worktree | Matcher::WorktreeOrphan)
}

/// Whether `path` lies inside a linked worktree the scan makes a region:
/// one of its ancestors below the longest scan root holding it, not the
/// home, is recognized as [`Recognized::Linked`]; an orphan does not count.
/// The walk's own answer without a walk, for a listed item's class.
pub(crate) fn inside(path: &Path, roots: &[&Path], home: &Path) -> bool {
    let Some(root) = roots.iter().filter(|r| path.starts_with(r)).max_by_key(|r| r.as_os_str().len()) else {
        return false;
    };
    path.ancestors()
        .skip(1)
        .take_while(|dir| *dir != *root && dir.starts_with(root))
        .any(|dir| dir != home && matches!(recognize(dir), Some(Recognized::Linked(_))))
}

/// What the scan knows of a linked worktree without a git probe: its
/// branch from `<gitdir>/HEAD` and its lock. The probes' fields stay absent.
pub fn state(linked: &Linked) -> WorktreeState {
    let head = read_line(&linked.gitdir.join("HEAD"));
    let (branch, detached) = match head.as_deref() {
        Some(head) => match head.strip_prefix("ref:") {
            Some(name) => {
                let name = name.trim();
                (Some(name.strip_prefix("refs/heads/").unwrap_or(name).to_owned()), Some(false))
            }
            None if is_hex(head) => (None, Some(true)),
            None => (None, None),
        },
        None => (None, None),
    };
    WorktreeState {
        linked: linked.linked,
        branch,
        detached,
        clean: None,
        ignored_content: None,
        own_commits: None,
        submodules: None,
        locked: Some(fs::symlink_metadata(linked.gitdir.join("locked")).is_ok()),
        in_use: None,
        idle_days: None,
        probe: WorktreeProbe::Unknown,
    }
}

/// The hint a worktree's finding shows: why it is not selected, and what
/// to run.
pub fn hint(found: &Recognized) -> String {
    match found {
        Recognized::Orphan { .. } => ORPHAN_HINT.to_owned(),
        Recognized::Linked(Linked { linked: false, main, .. }) => {
            format!("moved or copied from a worktree of {}: git worktree repair relinks it", main.display())
        }
        Recognized::Linked(Linked { harness: OriginHarness::Git, main, .. }) => {
            format!("yours: a linked worktree of {}; list it to remove it", main.display())
        }
        Recognized::Linked(Linked { main, .. }) => {
            format!("a linked worktree of {}; removed with git worktree remove", main.display())
        }
    }
}

/// What removing an eligible worktree does to its branch, for its hint.
fn removed(branch: Option<&str>) -> String {
    match branch {
        Some(branch) => {
            format!("removed with git worktree remove; branch {branch} is kept, git branch -d to drop it")
        }
        None => "removed with git worktree remove; its HEAD is detached and no other ref loses a commit".to_owned(),
    }
}

/// What the six conditions need besides the walk (ADR "Agent footprint:
/// worktrees, harness data and bounded consent for agents", §2, "Class").
pub struct Probing<'a> {
    pub now: SystemTime,
    /// `--worktree-min-age`: the idle window of condition 3.
    pub min_age: Duration,
    /// Condition 2's in-use check reads it, once per scan.
    pub processes: &'a dyn ProcessTable,
    /// Runs the git probes, in parallel across the candidates.
    pub commands: &'a (dyn CommandRunner + Sync),
    /// Where [`git::locate`] looks for a git.
    pub tool_dirs: Vec<PathBuf>,
    /// Each probe's timeout.
    pub timeout: Duration,
    /// Rules switched off in `settings.json`: a worktree of one is never probed.
    pub disabled_rules: &'a [String],
    /// `Scan` for every scan and clean; `Footprint` only for `ddc agent
    /// footprint`.
    pub mode: Assess,
}

/// How far a worktree is assessed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Assess {
    /// The ADR's order: the probes of 4 to 6 run only for a worktree that
    /// passed 1 to 3.
    Scan,
    /// `ddc agent footprint` (story "ddc footprint: a scoped project
    /// report"): a worktree that failed only 2 or 3 is probed for 4 to 6
    /// too, so the report can tell what it frees once its session ends.
    /// Its class stays `review` whatever the probes say.
    Footprint,
}

/// Why a `git.worktree` finding is not selected, in the conditions' order
/// (ADR "Agent footprint", §2): what `ddc agent footprint` sorts by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Held {
    /// Condition 1: a harness ddc does not select from yet.
    NotAHarness(OriginHarness),
    /// Copied or moved by hand: its gitdir does not name it.
    NotLinked,
    /// The person's own (`origin.harness: git`).
    Yours,
    /// Its rule is switched off in `settings.json`.
    RuleDisabled,
    /// Condition 2: `<gitdir>/locked` exists.
    Locked,
    /// Condition 2: a process of the current user works inside it.
    InUse,
    /// Condition 3: used within `--worktree-min-age`.
    Idle,
    /// Condition 3: nothing tells when it was last used.
    ActivityUnknown,
    /// Condition 4.
    Submodules,
    /// Condition 5: uncommitted changes or untracked files.
    Dirty,
    /// Condition 5: ignored content, or files git does not check.
    IgnoredContent,
    /// Condition 6.
    OwnCommits,
    /// A probe failed: fail closed.
    ProbeUnknown,
}

/// The pid in a `git worktree lock` reason, wherever `(pid N` stands in it:
/// Claude Code writes `claude agent <name> (pid N start …)` and `claude
/// session <name> (pid N …)`. None when the reason names no pid.
pub fn lock_pid(reason: &str) -> Option<u32> {
    let at = reason.find("(pid ")? + "(pid ".len();
    let digits: String = reason[at..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok().filter(|&pid| pid > 0)
}

/// The reason in `<gitdir>/locked`, its first line; None without the file.
pub fn lock_reason(linked: &Linked) -> Option<String> {
    if fs::symlink_metadata(linked.gitdir.join("locked")).is_err() {
        return None;
    }
    Some(read_line(&linked.gitdir.join("locked")).unwrap_or_default())
}

/// The most ignored paths a worktree's state lists.
pub const MAX_IGNORED: usize = 20;

/// An ignored file larger than this is never compared with the main
/// checkout's: it counts as content of its own.
pub const MAX_COMPARED: u64 = 64 * 1024 * 1024;

/// A worktree that passed conditions 1 to 3, waiting for its probes.
struct Candidate {
    index: usize,
    path: PathBuf,
    linked: Linked,
    state: WorktreeState,
    /// Footprint only: it failed condition 2 or 3, whose hint it keeps.
    early: bool,
}

/// Where a candidate's probes ended.
enum Verdict {
    /// All six conditions hold.
    Eligible,
    /// The first condition that failed: its number, and why, in words.
    Kept(u8, String),
    /// A probe failed: fail closed.
    Unknown(String),
}

/// The first of conditions 4 to 6 that does not hold.
pub(crate) enum Failed {
    /// The condition's number, and why, in words.
    Kept(u8, String),
    /// A probe of that condition failed: fail closed.
    Unknown(u8, String),
}

/// The claimed findings inside a worktree that ddc removes on its own, for
/// condition 5's accounting: asked only when git lists ignored paths.
pub(crate) type Owners<'o> = dyn Fn() -> Cow<'o, [PathBuf]> + Sync + 'o;

/// The six conditions, in the ADR's order, for every linked worktree of a
/// known harness, stopping at the first failure, which its state records
/// and its hint names. One that holds them all becomes `rebuildable` and is
/// selected; any other stays `review`. A worktree copied or moved by hand
/// and a person's own get no condition and no probe: their hints say why.
/// Conditions 1 to 3 read files and the process table; the git probes of 4
/// to 6 run only for the worktrees that passed them, in parallel.
pub(crate) fn assess(found: &mut [Classified], worktrees: &HashMap<PathBuf, Recognized>, probing: &Probing<'_>) {
    let mut users: Option<Users> = None;
    let mut candidates = Vec::new();
    let footprint = probing.mode == Assess::Footprint;
    for (index, c) in found.iter_mut().enumerate() {
        if !is_worktree_rule(c.rule) {
            continue;
        }
        let Some(Recognized::Linked(linked)) = worktrees.get(&c.owner.path) else { continue };
        let Some(state) = c.worktree.as_mut() else { continue };
        if !linked.linked {
            c.held.push(Held::NotLinked);
            continue;
        }
        if linked.harness == OriginHarness::Git {
            c.held.push(Held::Yours);
            continue;
        }
        if probing.disabled_rules.iter().any(|r| r == c.rule.id) {
            c.held.push(Held::RuleDisabled);
            continue;
        }
        let kept = |reason: &str| Some(kept_hint(&linked.main, reason));
        // 1. A known harness.
        if linked.harness != OriginHarness::ClaudeCode {
            c.hint =
                kept(&format!("made by {}, whose worktrees ddc does not select yet", harness_name(linked.harness)));
            c.held.push(Held::NotAHarness(linked.harness));
            continue;
        }
        // 2. Unlocked and not in use.
        if state.locked == Some(true) {
            c.hint = kept("locked; git worktree unlock releases it when no session needs it");
            c.held.push(Held::Locked);
        }
        if c.held.is_empty() || footprint {
            let users = users.get_or_insert_with(|| {
                let table = Users::read(probing.processes);
                // Footprint is asked from inside the worktree, or about it by path.
                match i32::try_from(std::process::id()) {
                    Ok(own) if footprint => table.without(own),
                    _ => table,
                }
            });
            if let Some(user) = users.of(&[c.owner.path.as_path()]) {
                state.in_use = Some(true);
                if c.held.is_empty() {
                    c.hint = kept(&in_use(&user));
                }
                c.held.push(Held::InUse);
            } else {
                state.in_use = Some(false);
            }
        }
        if !c.held.is_empty() && !footprint {
            continue;
        }
        // 3. Idle by its own activity.
        match project::last_activity(&c.owner.path) {
            None => {
                if c.held.is_empty() {
                    c.hint = kept("nothing tells when it was last used");
                }
                c.held.push(Held::ActivityUnknown);
            }
            Some(last) => {
                let idle = probing.now.duration_since(last).unwrap_or_default();
                state.idle_days = Some(idle.as_secs() / 86_400);
                if idle < probing.min_age {
                    let days = idle.as_secs() / 86_400;
                    let window = probing.min_age.as_secs() / 86_400;
                    if c.held.is_empty() {
                        c.hint =
                            kept(&format!("used {days} days ago, within the {window}-day window (--worktree-min-age)"));
                    }
                    c.held.push(Held::Idle);
                }
            }
        }
        let early = !c.held.is_empty();
        if early && !footprint {
            continue;
        }
        candidates.push(Candidate {
            index,
            path: c.owner.path.clone(),
            linked: linked.clone(),
            state: state.clone(),
            early,
        });
    }
    if candidates.is_empty() {
        return;
    }
    let owners = removable_owners(found);
    let owners: &Owners<'_> = &|| Cow::Borrowed(owners.as_slice());
    let exe = git::locate(probing.commands, &probing.tool_dirs, probing.timeout);
    let (commands, timeout) = (probing.commands, probing.timeout);
    // Each repository's references are asked once, by the first of its
    // worktrees that reaches condition 6.
    let repos: HashMap<PathBuf, OnceLock<Result<Refs, String>>> =
        candidates.iter().map(|c| (c.linked.main.clone(), OnceLock::new())).collect();
    let done: Vec<(Candidate, Verdict)> = candidates
        .into_par_iter()
        .map(|mut cand| {
            let verdict = match &exe {
                None => Verdict::Unknown(NO_GIT.to_owned()),
                Some(exe) => {
                    let git = Git::new(commands, exe.clone(), timeout);
                    let refs = &repos[&cand.linked.main];
                    match conditions(&git, &cand.path, &cand.linked, owners, refs, &mut cand.state) {
                        Ok(()) => Verdict::Eligible,
                        Err(Failed::Kept(n, reason)) => Verdict::Kept(n, reason),
                        Err(Failed::Unknown(_, why)) => Verdict::Unknown(why),
                    }
                }
            };
            (cand, verdict)
        })
        .collect();
    for (mut cand, verdict) in done {
        let c = &mut found[cand.index];
        cand.state.probe =
            if matches!(verdict, Verdict::Unknown(_)) { WorktreeProbe::Unknown } else { WorktreeProbe::Known };
        match &verdict {
            Verdict::Eligible => {}
            Verdict::Kept(n, reason) => c.held.push(match n {
                4 => Held::Submodules,
                5 if reason.starts_with("it has uncommitted") => Held::Dirty,
                5 => Held::IgnoredContent,
                _ => Held::OwnCommits,
            }),
            Verdict::Unknown(_) => c.held.push(Held::ProbeUnknown),
        }
        // A footprint candidate that failed 2 or 3 keeps that hint and its class.
        if !cand.early {
            c.hint = Some(match verdict {
                Verdict::Eligible => {
                    c.class = RiskClass::Rebuildable;
                    let main = cand.linked.main.display();
                    let branch = cand.state.branch.as_deref().filter(|_| cand.state.detached == Some(false));
                    format!("a linked worktree of {main} that nothing needs any more; {}", removed(branch))
                }
                Verdict::Kept(_, reason) => kept_hint(&cand.linked.main, &reason),
                Verdict::Unknown(why) => kept_hint(&cand.linked.main, &format!("git could not tell its state: {why}")),
            });
        }
        c.worktree = Some(cand.state);
    }
}

/// Conditions 4 to 6 through git; the first that fails, or the probe's
/// failure, with its number.
fn conditions(
    git: &Git<'_>,
    path: &Path,
    linked: &Linked,
    owners: &Owners<'_>,
    refs: &OnceLock<Result<Refs, String>>,
    state: &mut WorktreeState,
) -> Result<(), Failed> {
    let unknown = |n: u8| move |why: String| Failed::Unknown(n, why);
    // 4. No initialized submodule: `git worktree remove` refuses one, and
    // ddc never passes `--force`.
    let modules = crate::stats::read_dir(linked.gitdir.join("modules")).is_ok_and(|mut rd| rd.next().is_some());
    let initialized = modules
        || git.submodule_paths(path).known().map_err(unknown(4))?.iter().any(|p| exists(&path.join(p).join(".git")));
    state.submodules = Some(initialized);
    if initialized {
        return Err(Failed::Kept(4, "a submodule is initialized in it, and git worktree remove refuses that".into()));
    }
    // 5. Clean, ignored content included.
    let status = git.status(path).known().map_err(unknown(5))?;
    state.clean = Some(!status.dirty);
    if status.dirty {
        return Err(Failed::Kept(5, "it has uncommitted changes or untracked files".to_owned()));
    }
    // Tracked files git does not compare (skip-worktree, assume-unchanged)
    // look clean with a local edit, which `git worktree remove` deletes.
    let unwatched: Vec<PathBuf> =
        git.unwatched(path).known().map_err(unknown(5))?.iter().map(|rel| path.join(rel)).collect();
    let unaccounted = if status.ignored.is_empty() {
        Vec::new()
    } else {
        unaccounted(path, &linked.main, &status.ignored, &owners())
    };
    let unwatched_first = !unwatched.is_empty();
    let held: Vec<PathBuf> = unwatched.into_iter().chain(unaccounted).collect();
    if let Some(first) = held.first() {
        let shown = first.strip_prefix(path).unwrap_or(first).display().to_string();
        let count = held.len() as u64;
        let more = if count > 1 { format!(" and {} more", count - 1) } else { String::new() };
        state.ignored_content = Some(IgnoredContent { paths: held.into_iter().take(MAX_IGNORED).collect(), count });
        return Err(Failed::Kept(
            5,
            if unwatched_first {
                format!("it holds files git does not check (skip-worktree or assume-unchanged): {shown}{more}")
            } else {
                format!("it holds ignored content no artifact accounts for: {shown}{more}")
            },
        ));
    }
    // 6. No commit of its own, against the references that survive it.
    let head = git.head(path).known().map_err(unknown(6))?;
    let refs = refs.get_or_init(|| git.refs(&linked.main).known()).as_ref().map_err(|why| unknown(6)(why.clone()))?;
    let survivors = refs.survivors(path, head.branch.as_deref());
    let own = git.own_commits(&linked.main, &head.commit, &survivors).known().map_err(unknown(6))?;
    state.own_commits = Some(own);
    if own > 0 {
        let commits = if own == 1 { "commit" } else { "commits" };
        return Err(Failed::Kept(6, format!("{own} {commits} that no branch, tag, remote or other worktree holds")));
    }
    Ok(())
}

/// What a re-check of one worktree runs with: at the plan of a person's
/// list, in its dry run, and right before the executor's command.
pub struct Recheck<'a> {
    pub commands: &'a dyn CommandRunner,
    /// Where [`git::locate`] looks for a git.
    pub tool_dirs: Vec<PathBuf>,
    /// Each probe's timeout.
    pub timeout: Duration,
    pub now: SystemTime,
    /// `--worktree-min-age`: condition 3's window.
    pub min_age: Duration,
}

/// A worktree the re-check let through: what git removes it from, and with
/// which git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removable {
    /// The main checkout, derived again from disk.
    pub main: PathBuf,
    pub git: PathBuf,
}

/// The six conditions again for the `git.worktree` finding at `path`,
/// through the scan's own probe code (ADR "Agent footprint", §2, and
/// decision 6 of story "Worktree findings and provenance"). `person`: a
/// person listed or confirmed it, which waives conditions 1 (a known
/// harness) and 3 (idle); 2, 4, 5 and 6 hold whoever asked, since `git
/// worktree remove` would delete unique ignored content or the only copy of
/// a commit. Condition 2's in-use half is the guard's own check. `main` is
/// the main checkout the scan recorded (`None` in a dry run, which has no
/// scan); a worktree whose main checkout is another now changed since the
/// scan. A copied or moved worktree is never removed: git lists another
/// folder. `owners` is asked only when git lists ignored paths.
pub fn recheck(
    path: &Path,
    main: Option<&Path>,
    person: bool,
    rc: &Recheck<'_>,
    owners: &Owners<'_>,
) -> Result<Removable, SkipReason> {
    let kept = |condition: u8, detail: String| Err(SkipReason::WorktreeKept { condition, detail });
    let linked = match recognize(path) {
        Some(Recognized::Linked(linked)) => linked,
        _ => return Err(SkipReason::RuleMismatch),
    };
    if !linked.linked {
        return Err(not_listed());
    }
    if main.is_some_and(|m| m != linked.main) {
        return Err(SkipReason::ChangedSinceScan);
    }
    // 1. A known harness.
    if !person && linked.harness != OriginHarness::ClaudeCode {
        return kept(
            1,
            match linked.harness {
                OriginHarness::Git => "yours: ddc removes it only when you list it".to_owned(),
                other => format!("made by {}, whose worktrees ddc does not select yet", harness_name(other)),
            },
        );
    }
    // 2. Unlocked.
    if exists(&linked.gitdir.join("locked")) {
        return kept(2, "locked; git worktree unlock releases it when no session needs it".to_owned());
    }
    // 3. Idle by its own activity.
    if !person {
        let Some(last) = project::last_activity(path) else {
            return kept(3, "nothing tells when it was last used".to_owned());
        };
        let idle = rc.now.duration_since(last).unwrap_or_default();
        if idle < rc.min_age {
            let (days, window) = (idle.as_secs() / 86_400, rc.min_age.as_secs() / 86_400);
            return kept(3, format!("used {days} days ago, within the {window}-day window (--worktree-min-age)"));
        }
    }
    let Some(exe) = git::locate(rc.commands, &rc.tool_dirs, rc.timeout) else {
        return Err(SkipReason::ToolNotReady { program: "git".to_owned(), detail: NO_GIT.to_owned() });
    };
    let probe = Git::new(rc.commands, exe.clone(), rc.timeout);
    let mut state = state(&linked);
    match conditions(&probe, path, &linked, owners, &OnceLock::new(), &mut state) {
        Ok(()) => Ok(Removable { main: linked.main, git: exe }),
        Err(Failed::Kept(n, why)) => kept(n, why),
        Err(Failed::Unknown(n, why)) => kept(n, format!("git could not tell its state: {why}")),
    }
}

/// Why nothing runs for a worktree git does not list: the location check
/// of safety ADR §6, realised as `tool-not-ready` (the 2026-11-01 amendment).
pub fn not_listed() -> SkipReason {
    SkipReason::ToolNotReady { program: "git".to_owned(), detail: NOT_LISTED.to_owned() }
}

/// [`not_listed`]'s detail.
pub const NOT_LISTED: &str = "no git lists this folder";

/// The claimed findings ddc removes on its own: `safe` or `rebuildable`
/// with method `delete`. Sorted; claims never nest.
pub(crate) fn removable_owners(found: &[Classified]) -> Vec<PathBuf> {
    let mut owners: Vec<PathBuf> = found
        .iter()
        .filter(|c| c.owner.kind == OwnerKind::Claim && c.rule.id != KEPT)
        .filter(|c| matches!(c.class, RiskClass::Safe | RiskClass::Rebuildable) && c.rule.removal == Removal::Delete)
        .map(|c| c.owner.path.clone())
        .collect();
    owners.sort();
    owners
}

/// The ignored paths of the worktree at `path` that nothing accounts for:
/// neither inside one of `owners` (sorted) nor a byte-identical copy of
/// the main checkout's file at the same place, which is what
/// `.worktreeinclude` makes. Absolute, in git's order.
fn unaccounted(path: &Path, main: &Path, ignored: &[String], owners: &[PathBuf]) -> Vec<PathBuf> {
    ignored
        .iter()
        .filter(|rel| {
            let at = path.join(rel);
            !(inside_owner(&at, owners) || same_bytes(&at, &main.join(rel.as_str())))
        })
        .map(|rel| path.join(rel.trim_end_matches('/')))
        .collect()
}

/// `at` lies in one of `owners`: the greatest owner not after it, since
/// the owners do not nest and a folder's descendants sort right after it.
fn inside_owner(at: &Path, owners: &[PathBuf]) -> bool {
    let i = owners.partition_point(|o| o.as_path() <= at);
    i > 0 && at.starts_with(&owners[i - 1])
}

/// Both are regular files of at most [`MAX_COMPARED`] bytes with the same
/// bytes. Each is opened without following a symlink and without blocking,
/// and must still be a regular file of that length once open: a FIFO
/// swapped in after the first look never blocks the scan.
pub(crate) fn same_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (fs::symlink_metadata(a), fs::symlink_metadata(b)) else { return false };
    if !ma.is_file() || !mb.is_file() || ma.len() != mb.len() || ma.len() > MAX_COMPARED {
        return false;
    }
    let open = |p: &Path| {
        crate::platform::fsops::open_nofollow_nonblock(p)
            .ok()
            .filter(|f| f.metadata().is_ok_and(|m| m.is_file() && m.len() == ma.len()))
    };
    let (Some(mut fa), Some(mut fb)) = (open(a), open(b)) else { return false };
    let (mut ba, mut bb) = (vec![0u8; 64 * 1024], vec![0u8; 64 * 1024]);
    loop {
        let (Ok(n), Ok(m)) = (read_full(&mut fa, &mut ba), read_full(&mut fb, &mut bb)) else { return false };
        if n != m || ba[..n] != bb[..m] {
            return false;
        }
        if n == 0 {
            return true;
        }
    }
}

/// Fills `buf` as far as the file goes; the bytes read.
fn read_full(f: &mut fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

fn in_use(user: &SkipReason) -> String {
    match user {
        SkipReason::InUse { pid, name } | SkipReason::InUseUnknownCwd { pid, name } => {
            format!("in use by {name} (pid {pid})")
        }
        _ => "in use".to_owned(),
    }
}

fn harness_name(harness: OriginHarness) -> &'static str {
    match harness {
        OriginHarness::ClaudeCode => "Claude Code",
        OriginHarness::Codex => "Codex",
        OriginHarness::Opencode => "opencode",
        OriginHarness::Cursor => "Cursor",
        OriginHarness::Git => "git",
    }
}

/// A harness's worktree that a condition keeps: the first one, named.
pub fn kept_hint(main: &Path, reason: &str) -> String {
    format!("a linked worktree of {}; kept: {reason}", main.display())
}

/// The reason in a hint [`kept_hint`] wrote for a worktree of `main`, for
/// the report to show; `None` for any other hint.
pub fn kept_reason<'a>(hint: &'a str, main: &Path) -> Option<&'a str> {
    hint.strip_prefix(&kept_hint(main, ""))
}

/// Why no probe ran: no git ddc may run.
pub const NO_GIT: &str = "no git to ask (the developer tools are not installed)";

/// The two worktree rules' ids.
pub const WORKTREE_RULE: &str = "git.worktree";
pub const ORPHAN_RULE: &str = "git.worktree-orphan";

/// `git.worktree`'s own hint, before a finding names its main checkout.
pub(crate) const WORKTREE_HINT: &str =
    "a linked worktree; removed with git worktree remove, its branch is kept, git branch -d to drop it";
/// `git.worktree-orphan`'s hint: both causes.
pub(crate) const ORPHAN_HINT: &str =
    "its repository is gone, or on a volume that is not mounted or was moved: git worktree repair";

/// `claude-code` when the gitdir carries a marker Claude Code writes or the
/// worktree lies under `<main>/.claude/worktrees/`; `codex` from Codex's
/// thread file; otherwise the person's own, `git`.
fn harness(path: &Path, gitdir: &Path, main: &Path) -> OriginHarness {
    let claude_base = read_line(&gitdir.join(CLAUDE_BASE)).is_some_and(|base| base.len() == 40 && is_hex(&base));
    // By spelling, else by identity: a root reached in another case on a
    // case-insensitive volume spells the worktree differently from `main`.
    let base = main.join(".claude").join("worktrees");
    let under_claude = path.strip_prefix(&base).is_ok_and(|rel| rel != Path::new(""))
        || path.ancestors().skip(1).any(|dir| same_file(dir, &base));
    if claude_base || exists(&gitdir.join(CLAUDE_DESKTOP)) || under_claude {
        OriginHarness::ClaudeCode
    } else if exists(&gitdir.join(CODEX_THREAD)) {
        OriginHarness::Codex
    } else {
        OriginHarness::Git
    }
}

/// An orphan's gitdir is gone, with its markers: only the path tells, a
/// folder of `.claude/worktrees/`.
fn orphan_harness(path: &Path) -> OriginHarness {
    let parent = path.parent();
    let named = |p: Option<&Path>, name: &str| p.and_then(Path::file_name).is_some_and(|n| n == name);
    if named(parent, "worktrees") && named(parent.and_then(Path::parent), ".claude") {
        OriginHarness::ClaudeCode
    } else {
        OriginHarness::Git
    }
}

fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn is_hex(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The first line of a small file, trimmed; `None` when it cannot be read
/// or is empty.
fn read_line(path: &Path) -> Option<String> {
    let mut text = String::new();
    crate::platform::fsops::open_read(path).ok()?.take(MAX_READ).read_to_string(&mut text).ok()?;
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| line.to_owned())
}

/// `target` as git means it: relative to `base` unless absolute, with `.`
/// and `..` resolved by hand (the folders may not exist).
fn resolve(base: &Path, target: &str) -> PathBuf {
    let target = Path::new(target);
    normalize(&if target.is_absolute() { target.to_path_buf() } else { base.join(target) })
}

/// `path` with `.` and `..` resolved by hand.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// Both name the same file, by `(dev, ino)`: case-folding and relative
/// links do not make a mismatch.
fn same_file(a: &Path, b: &Path) -> bool {
    use crate::platform::meta::{lstat, stat};
    match (stat(a), lstat(b)) {
        (Ok(a), Ok(b)) => a.id() == b.id(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lock_names_its_pid_wherever_it_stands() {
        assert_eq!(lock_pid("claude agent a1b2 (pid 4242 start 1730000000)"), Some(4242));
        assert_eq!(lock_pid("claude session fix-it (pid 77)"), Some(77));
        assert_eq!(lock_pid("(pid 9)"), Some(9));
        for reason in ["", "kept for Monday", "pid 12", "(pid )", "(pid x1)", "(pid 0)"] {
            assert_eq!(lock_pid(reason), None, "{reason:?}");
        }
    }

    #[test]
    fn resolve_follows_git_relative_paths() {
        let base = Path::new("/r/.git/worktrees/x");
        assert_eq!(resolve(base, "../.."), Path::new("/r/.git"));
        assert_eq!(resolve(base, "/abs/./y/.git"), Path::new("/abs/y/.git"));
        assert_eq!(resolve(base, "../../../wt/.git"), Path::new("/r/wt/.git"));
    }

    #[test]
    fn hex_is_digits_a_to_f_only() {
        assert!(is_hex("0123456789abcdefABCDEF"));
        assert!(!is_hex(""));
        assert!(!is_hex("ref: refs/heads/main"));
    }

    #[test]
    fn an_orphan_under_claude_worktrees_is_claude_codes() {
        assert_eq!(orphan_harness(Path::new("/p/.claude/worktrees/x")), OriginHarness::ClaudeCode);
        assert_eq!(orphan_harness(Path::new("/p/worktrees/x")), OriginHarness::Git);
        assert_eq!(orphan_harness(Path::new("/x")), OriginHarness::Git);
    }

    #[test]
    fn the_hint_says_whose_and_what_to_run() {
        let linked = |harness, linked| {
            Recognized::Linked(Linked {
                gitdir: PathBuf::from("/m/.git/worktrees/w"),
                main: "/m".into(),
                harness,
                linked,
            })
        };
        assert!(hint(&linked(OriginHarness::ClaudeCode, true)).starts_with("a linked worktree of /m;"));
        assert!(hint(&linked(OriginHarness::Git, true)).starts_with("yours: a linked worktree of /m;"));
        assert!(hint(&linked(OriginHarness::Git, false)).contains("git worktree repair"));
        assert_eq!(hint(&Recognized::Orphan { harness: OriginHarness::Git }), ORPHAN_HINT);
    }

    #[test]
    fn the_kept_reason_is_read_back_whatever_the_paths_hold() {
        let main = Path::new("/m; kept: x");
        let reason = "it holds ignored content no artifact accounts for: a; kept: b";
        assert_eq!(kept_reason(&kept_hint(main, reason), main), Some(reason));
        assert_eq!(kept_reason(&kept_hint(main, reason), Path::new("/n")), None);
        assert_eq!(
            kept_reason(&format!("a linked worktree of {}; removed with git worktree remove", main.display()), main),
            None
        );
    }

    /// Only regular files of at most `MAX_COMPARED` bytes are compared; a
    /// FIFO is never opened, so it cannot block the scan. Skipped where the
    /// scanner is not ported (Windows): no `mkfifo` there.
    #[test]
    fn only_small_regular_files_are_compared() {
        if crate::platform::skipped("only_small_regular_files_are_compared") {
            return;
        }
        let tree = Tree::new();
        tree.write("a", "same");
        tree.write("b", "same");
        tree.write("c", "diff");
        let (a, b, c) = (tree.root.join("a"), tree.root.join("b"), tree.root.join("c"));
        assert!(same_bytes(&a, &b));
        assert!(!same_bytes(&a, &c));
        assert!(!same_bytes(&a, &tree.root.join("missing")));
        let fifo = tree.root.join("fifo");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status().unwrap();
        assert!(made.success());
        assert!(!same_bytes(&fifo, &a), "a FIFO is never opened");
        assert!(!same_bytes(&a, &fifo));
        let big = |name: &str| {
            let p = tree.root.join(name);
            fs::File::create(&p).unwrap().set_len(MAX_COMPARED + 1).unwrap();
            p
        };
        assert!(!same_bytes(&big("big1"), &big("big2")), "larger than MAX_COMPARED");
    }

    /// A temporary folder, canonical, with helpers to make folders and files in it.
    struct Tree {
        _dir: tempfile::TempDir,
        root: PathBuf,
    }

    impl Tree {
        fn new() -> Self {
            let d = tempfile::tempdir().unwrap();
            let root = d.path().canonical().unwrap();
            Self { _dir: d, root }
        }
        fn dir(&self, rel: &str) -> PathBuf {
            let p = self.root.join(rel);
            fs::create_dir_all(&p).unwrap();
            p
        }
        fn write(&self, rel: &str, text: &str) {
            fs::write(self.root.join(rel), text).unwrap();
        }
        /// A worktree at `rel` of a bare repository `bare.git`, without git.
        fn worktree(&self, rel: &str) -> PathBuf {
            let name = Path::new(rel).file_name().unwrap().to_str().unwrap();
            self.dir(&format!("bare.git/worktrees/{name}"));
            self.write(&format!("bare.git/worktrees/{name}/commondir"), "../..\n");
            self.write(&format!("bare.git/worktrees/{name}/HEAD"), "garbage\n");
            self.dir(rel);
            let gitdir = self.root.join(format!("bare.git/worktrees/{name}"));
            self.write(&format!("{rel}/.git"), &format!("gitdir: {}\n", gitdir.display()));
            self.root.join(rel)
        }
    }

    /// What is not a worktree, without a git: no `.git` file, no `gitdir:`
    /// line, a gitdir that is a file or has no `commondir`, a missing gitdir
    /// that is not a worktree's. Skipped where the scanner is not ported
    /// (Windows): the paths are written as git writes them on Unix.
    #[test]
    fn only_a_git_file_into_a_gitdir_with_a_commondir_is_a_worktree() {
        if crate::platform::skipped("only_a_git_file_into_a_gitdir_with_a_commondir_is_a_worktree") {
            return;
        }
        let tree = Tree::new();
        let root = tree.root.clone();
        let dir = |rel: &str| tree.dir(rel);
        let write = |rel: &str, text: &str| tree.write(rel, text);
        dir("checkout/.git");
        assert_eq!(recognize(&root.join("checkout")), None, "a main checkout");
        assert_eq!(recognize(&dir("plain")), None, "no .git");
        dir("no-line");
        write("no-line/.git", "not git\n");
        assert_eq!(recognize(&root.join("no-line")), None);
        write("a-file", "x");
        dir("to-file");
        write("to-file/.git", &format!("gitdir: {}\n", root.join("a-file").display()));
        assert_eq!(recognize(&root.join("to-file")), None, "a gitdir that is a file");
        dir("modules/sub");
        dir("sub");
        write("sub/.git", "gitdir: ../modules/sub\n");
        assert_eq!(recognize(&root.join("sub")), None, "a submodule's gitdir has no commondir");
        dir("copied/sub");
        write("copied/sub/.git", "gitdir: ../../.git/modules/sub\n");
        assert_eq!(recognize(&root.join("copied/sub")), None, "a submodule copied out of its superproject");
        dir("elsewhere");
        write("elsewhere/.git", "gitdir: /nowhere/at/all\n");
        assert_eq!(recognize(&root.join("elsewhere")), None, "a missing gitdir that is no worktree's");
        dir("gone");
        write("gone/.git", "gitdir: ../repo/.git/worktrees/gone\n");
        assert_eq!(recognize(&root.join("gone")), Some(Recognized::Orphan { harness: OriginHarness::Git }));
        tree.worktree("b");
        let Some(Recognized::Linked(linked)) = recognize(&root.join("b")) else { panic!("b is a worktree") };
        assert_eq!(linked.main, root.join("bare.git"), "a bare repository is its own main");
        assert!(!linked.linked, "no back-pointer");
        assert_eq!(linked.harness, OriginHarness::Git);
        let state = state(&linked);
        assert_eq!((state.branch, state.detached, state.locked), (None, None, Some(false)), "an unreadable HEAD");
        assert!(!inside(&root.join("b/x"), &[Path::new("/elsewhere")], &root), "outside every root");
        assert!(inside(&root.join("b/x"), &[root.as_path()], Path::new("/")));
        assert!(!inside(&root.join("b"), &[root.join("b").as_path()], Path::new("/")), "the scan root itself");
        assert!(!inside(&root.join("gone/x"), &[root.as_path()], Path::new("/")), "an orphan lifts nothing");
    }

    /// Another rule's known region stays the region it is: a worktree there
    /// is not recorded, so nothing inside it loses a downgrade for it.
    #[test]
    fn a_known_region_is_never_a_worktree_region() {
        use crate::class::{Category, Removal, RiskClass};
        use crate::rules::{Catalog, Clones, KEPT, Scope};
        if crate::platform::skipped("a_known_region_is_never_a_worktree_region") {
            return;
        }
        let rule = |id, matcher| Rule {
            id,
            category: Category::Personal,
            class: RiskClass::UserData,
            removal: Removal::None,
            matcher,
            lockfiles: None,
            owners: &[],
            hint: "",
            listed: true,
            clones: Clones::Never,
            scope: Scope::Item,
            origin: None,
            listed_only: false,
        };
        let rules: &'static [Rule] = Box::leak(Box::new([
            rule(KEPT, Matcher::Synthetic),
            rule("git.worktree", Matcher::Worktree),
            rule("region", Matcher::KnownRegion(&["dl"])),
        ]));
        let tree = Tree::new();
        let region = tree.worktree("dl");
        let plain = tree.worktree("plain");
        let catalog = Catalog::from_rules(rules, &tree.root);
        let has_git = |n: &str| n == ".git";
        assert!(catalog.worktree_region(&region, 1, has_git).is_none());
        assert_eq!(catalog.worktree_region(&plain, 1, has_git).map(|(tag, _)| tag), Some(1));
        assert!(catalog.worktree_region(&plain, 0, has_git).is_none(), "a scan root");
    }
}
