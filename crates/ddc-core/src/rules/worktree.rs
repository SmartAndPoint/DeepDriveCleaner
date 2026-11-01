//! Linked git worktrees (ADR "Agent footprint: worktrees, harness data and
//! bounded consent for agents", §2 and its amendment of 2026-11-01): what a
//! folder whose `.git` is a file is, read from the files git keeps, never by
//! running git. The rules `git.worktree` and `git.worktree-orphan` make such
//! a folder a region, so the other rules keep working inside it.

use std::fs;
use std::io::{ErrorKind, Read as _};
use std::path::{Component, Path, PathBuf};

use super::{Matcher, Rule};
use crate::model::{OriginHarness, WorktreeProbe, WorktreeState};
use crate::report::project::git_dir;

/// What a folder whose `.git` file names a gitdir is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recognized {
    /// A linked worktree: its gitdir exists and has a `commondir`.
    Linked(Linked),
    /// Its gitdir does not exist: the repository was removed or moved, or
    /// lies on a volume that is not mounted. The folder is a copy of files.
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
        Ok(md) if md.is_dir() => named.canonicalize().ok()?,
        Ok(_) => return None,
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Some(Recognized::Orphan { harness: orphan_harness(path) });
        }
        Err(_) => return None,
    };
    // A submodule's gitdir (`.git/modules/<n>`) has no `commondir`: never a worktree.
    let common = resolve(&gitdir, read_line(&gitdir.join("commondir"))?.as_str());
    let main = if common.file_name().is_some_and(|n| n == ".git") { common.parent()?.to_path_buf() } else { common };
    let main = main.canonicalize().unwrap_or(main);
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

/// Whether `path` lies inside a folder the scan makes a worktree region:
/// one of its ancestors below the longest scan root holding it, not the
/// home, is recognized. The walk's own answer without a walk, for a listed
/// item's class.
pub(crate) fn inside(path: &Path, roots: &[&Path], home: &Path) -> bool {
    let Some(root) = roots.iter().filter(|r| path.starts_with(r)).max_by_key(|r| r.as_os_str().len()) else {
        return false;
    };
    path.ancestors()
        .skip(1)
        .take_while(|dir| *dir != *root && dir.starts_with(root))
        .any(|dir| dir != home && recognize(dir).is_some())
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
    let joined = if target.is_absolute() { target.to_path_buf() } else { base.join(target) };
    let mut out = PathBuf::new();
    for c in joined.components() {
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
    use crate::platform::meta::file_id;
    match (fs::metadata(a), fs::symlink_metadata(b)) {
        (Ok(a), Ok(b)) => matches!((file_id(&a), file_id(&b)), (Ok(x), Ok(y)) if x == y),
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

    /// What is not a worktree, without a git: no `.git` file, no `gitdir:`
    /// line, a gitdir that is a file or has no `commondir`.
    #[test]
    fn only_a_git_file_into_a_gitdir_with_a_commondir_is_a_worktree() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().canonicalize().unwrap();
        let dir = |rel: &str| {
            let p = root.join(rel);
            fs::create_dir_all(&p).unwrap();
            p
        };
        let write = |rel: &str, text: &str| fs::write(root.join(rel), text).unwrap();
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
        dir("bare.git/worktrees/b");
        write("bare.git/worktrees/b/commondir", "../..\n");
        write("bare.git/worktrees/b/HEAD", "garbage\n");
        dir("b");
        write("b/.git", &format!("gitdir: {}\n", root.join("bare.git/worktrees/b").display()));
        let Some(Recognized::Linked(linked)) = recognize(&root.join("b")) else { panic!("b is a worktree") };
        assert_eq!(linked.main, root.join("bare.git"), "a bare repository is its own main");
        assert!(!linked.linked, "no back-pointer");
        assert_eq!(linked.harness, OriginHarness::Git);
        let state = state(&linked);
        assert_eq!((state.branch, state.detached, state.locked), (None, None, Some(false)), "an unreadable HEAD");
        assert!(!inside(&root.join("b/x"), &[Path::new("/elsewhere")], &root), "outside every root");
        assert!(inside(&root.join("b/x"), &[root.as_path()], Path::new("/")));
        assert!(!inside(&root.join("b"), &[root.join("b").as_path()], Path::new("/")), "the scan root itself");
    }
}
