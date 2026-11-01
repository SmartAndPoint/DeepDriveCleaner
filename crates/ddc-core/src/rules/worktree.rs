//! Linked git worktrees (ADR "Agent footprint: worktrees, harness data and
//! bounded consent for agents", §2 and its amendment of 2026-11-01): what a
//! folder whose `.git` is a file is, read from the files git keeps, never by
//! running git. The rules `git.worktree` and `git.worktree-orphan` make such
//! a folder a region, so the other rules keep working inside it.

use crate::platform::Canonical as _;
use std::collections::HashMap;
use std::fs;
use std::io::{ErrorKind, Read as _};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use rayon::prelude::*;

use super::{Classified, KEPT, Matcher, Rule};
use crate::class::{Removal, RiskClass};
use crate::exec::guard::Users;
use crate::exec::{CommandRunner, ProcessTable, SkipReason};
use crate::git::{self, Git};
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
        Ok(md) if md.is_dir() => named.canonical().ok()?,
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
    let main = if common.file_name().is_some_and(|n| n == ".git") { common.parent()?.to_path_buf() } else { common };
    let main = main.canonical().unwrap_or(main);
    let linked = read_line(&gitdir.join("gitdir"))
        .map(|back| resolve(&gitdir, &back))
        .is_some_and(|back| same_file(&back, &dot_git));
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
            format!("yours: a linked worktree of {}; {LATER}", main.display())
        }
        Recognized::Linked(Linked { main, .. }) => format!("a linked worktree of {}; {LATER}", main.display()),
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
}

/// The most ignored paths a worktree's state lists.
pub const MAX_IGNORED: usize = 20;

/// A worktree that passed conditions 1 to 3, waiting for its probes.
struct Candidate {
    index: usize,
    path: PathBuf,
    linked: Linked,
    state: WorktreeState,
}

/// Where a candidate's probes ended.
enum Verdict {
    /// All six conditions hold.
    Eligible,
    /// The first condition that failed, in words.
    Kept(String),
    /// A probe failed: fail closed.
    Unknown(String),
}

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
    for (index, c) in found.iter_mut().enumerate() {
        if !is_worktree_rule(c.rule) {
            continue;
        }
        let Some(Recognized::Linked(linked)) = worktrees.get(&c.owner.path) else { continue };
        let Some(state) = c.worktree.as_mut() else { continue };
        if !linked.linked || linked.harness == OriginHarness::Git {
            continue;
        }
        let kept = |reason: &str| Some(kept_hint(&linked.main, reason));
        // 1. A known harness.
        if linked.harness != OriginHarness::ClaudeCode {
            c.hint =
                kept(&format!("made by {}, whose worktrees ddc does not select yet", harness_name(linked.harness)));
            continue;
        }
        // 2. Unlocked and not in use.
        if state.locked == Some(true) {
            c.hint = kept("locked; git worktree unlock releases it when no session needs it");
            continue;
        }
        let users = users.get_or_insert_with(|| Users::read(probing.processes));
        if let Some(user) = users.of(&[c.owner.path.as_path()]) {
            state.in_use = Some(true);
            c.hint = kept(&in_use(&user));
            continue;
        }
        state.in_use = Some(false);
        // 3. Idle by its own activity.
        let Some(last) = project::last_activity(&c.owner.path) else {
            c.hint = kept("nothing tells when it was last used");
            continue;
        };
        let idle = probing.now.duration_since(last).unwrap_or_default();
        state.idle_days = Some(idle.as_secs() / 86_400);
        if idle < probing.min_age {
            let days = idle.as_secs() / 86_400;
            let window = probing.min_age.as_secs() / 86_400;
            c.hint = kept(&format!("used {days} days ago, within the {window}-day window (--worktree-min-age)"));
            continue;
        }
        candidates.push(Candidate { index, path: c.owner.path.clone(), linked: linked.clone(), state: state.clone() });
    }
    if candidates.is_empty() {
        return;
    }
    let owners = removable_owners(found);
    let exe = git::locate(probing.commands, &probing.tool_dirs, probing.timeout);
    let (commands, timeout) = (probing.commands, probing.timeout);
    let done: Vec<(Candidate, Verdict)> = candidates
        .into_par_iter()
        .map(|mut cand| {
            let verdict = match &exe {
                None => Verdict::Unknown(NO_GIT.to_owned()),
                Some(exe) => {
                    let git = Git::new(commands, exe.clone(), timeout);
                    match conditions(&git, &cand.path, &cand.linked, &owners, &mut cand.state) {
                        Ok(None) => Verdict::Eligible,
                        Ok(Some(reason)) => Verdict::Kept(reason),
                        Err(why) => Verdict::Unknown(why),
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
        c.hint = Some(match verdict {
            Verdict::Eligible => {
                c.class = RiskClass::Rebuildable;
                format!("a linked worktree of {} that nothing needs any more; {LATER}", cand.linked.main.display())
            }
            Verdict::Kept(reason) => kept_hint(&cand.linked.main, &reason),
            Verdict::Unknown(why) => kept_hint(&cand.linked.main, &format!("git could not tell its state: {why}")),
        });
        c.worktree = Some(cand.state);
    }
}

/// Conditions 4 to 6 through git; the reason the first failing one gives,
/// or the probe's failure.
fn conditions(
    git: &Git<'_>,
    path: &Path,
    linked: &Linked,
    owners: &[PathBuf],
    state: &mut WorktreeState,
) -> Result<Option<String>, String> {
    // 4. No initialized submodule: `git worktree remove` refuses one, and
    // ddc never passes `--force`.
    let modules = crate::stats::read_dir(linked.gitdir.join("modules")).is_ok_and(|mut rd| rd.next().is_some());
    let initialized = modules || git.submodule_paths(path).known()?.iter().any(|p| exists(&path.join(p).join(".git")));
    state.submodules = Some(initialized);
    if initialized {
        return Ok(Some("a submodule is initialized in it, and git worktree remove refuses that".to_owned()));
    }
    // 5. Clean, ignored content included.
    let status = git.status(path).known()?;
    state.clean = Some(!status.dirty);
    if status.dirty {
        return Ok(Some("it has uncommitted changes or untracked files".to_owned()));
    }
    let unaccounted = unaccounted(path, &linked.main, &status.ignored, owners);
    if let Some(first) = unaccounted.first() {
        let shown = first.strip_prefix(path).unwrap_or(first).display().to_string();
        let count = unaccounted.len() as u64;
        let more = if count > 1 { format!(" and {} more", count - 1) } else { String::new() };
        state.ignored_content =
            Some(IgnoredContent { paths: unaccounted.into_iter().take(MAX_IGNORED).collect(), count });
        return Ok(Some(format!("it holds ignored content no artifact accounts for: {shown}{more}")));
    }
    // 6. No commit of its own, against the references that survive it.
    let head = git.head(path).known()?;
    let survivors = git.survivors(&linked.main, path, head.branch.as_deref()).known()?;
    let own = git.own_commits(&linked.main, &head.commit, &survivors).known()?;
    state.own_commits = Some(own);
    Ok((own > 0).then(|| {
        let commits = if own == 1 { "commit" } else { "commits" };
        format!("{own} {commits} that no branch, tag, remote or other worktree holds")
    }))
}

/// The claimed findings ddc removes on its own: `safe` or `rebuildable`
/// with method `delete`. Sorted; claims never nest.
fn removable_owners(found: &[Classified]) -> Vec<PathBuf> {
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

/// Both are regular files with the same bytes.
fn same_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (fs::symlink_metadata(a), fs::symlink_metadata(b)) else { return false };
    if !ma.is_file() || !mb.is_file() || ma.len() != mb.len() {
        return false;
    }
    let (Ok(mut fa), Ok(mut fb)) = (fs::File::open(a), fs::File::open(b)) else { return false };
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
fn kept_hint(main: &Path, reason: &str) -> String {
    format!("a linked worktree of {}; kept: {reason}", main.display())
}

/// Why no probe ran: no git ddc may run.
const NO_GIT: &str = "no git to ask (the developer tools are not installed)";

/// Removal through git comes with a later release (the story's PR 3).
const LATER: &str = "ddc removes worktrees through git worktree remove in a later release";
/// `git.worktree`'s own hint, before a finding names its main checkout.
pub(crate) const WORKTREE_HINT: &str =
    "a linked worktree; ddc removes worktrees through git worktree remove in a later release";
/// `git.worktree-orphan`'s hint: both causes.
pub(crate) const ORPHAN_HINT: &str =
    "its repository is gone, or on a volume that is not mounted or was moved: git worktree repair";

/// `claude-code` when the gitdir carries a marker Claude Code writes or the
/// worktree lies under `<main>/.claude/worktrees/`; `codex` from Codex's
/// thread file; otherwise the person's own, `git`.
fn harness(path: &Path, gitdir: &Path, main: &Path) -> OriginHarness {
    let claude_base = read_line(&gitdir.join(CLAUDE_BASE)).is_some_and(|base| base.len() == 40 && is_hex(&base));
    let under_claude = path.strip_prefix(main.join(".claude").join("worktrees")).is_ok_and(|rel| rel != Path::new(""));
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
    fs::File::open(path).ok()?.take(MAX_READ).read_to_string(&mut text).ok()?;
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
