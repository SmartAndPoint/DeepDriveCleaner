//! Linked git worktrees (ADR "Agent footprint: worktrees, harness data and
//! bounded consent for agents", §2 and its amendment of 2026-11-01): what a
//! folder whose `.git` is a file is, read from the files git keeps, never by
//! running git. The rules `git.worktree` and `git.worktree-orphan` make such
//! a folder a region, so the other rules keep working inside it.

use crate::platform::Canonical as _;
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
