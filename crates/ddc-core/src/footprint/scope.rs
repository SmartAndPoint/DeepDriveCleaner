//! Which checkouts belong to the project at ROOT, and what to walk to read
//! them (story "ddc footprint: a scoped project report", revision of
//! 2026-11-08): what `ddc agent footprint` and `ddc agent clean` share.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::exec::CommandRunner;
use crate::git::{self, Git};
use crate::platform::Canonical as _;
use crate::report::recheck::Reach;
use crate::rules::worktree::{self, Recognized};

use super::Refused;

/// The project at ROOT: its main checkout and every checkout git lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    /// The folder asked about, canonical.
    pub root: PathBuf,
    /// The main checkout, or the bare repository.
    pub main: PathBuf,
    pub bare: bool,
    /// Why `git worktree list` gave no answer; `None` when it did.
    pub listing_error: Option<String>,
    /// Canonical; the main checkout first unless bare, then git's order.
    pub checkouts: Vec<PathBuf>,
    /// Worktrees git lists whose folder is gone or does not resolve.
    pub missing: Vec<PathBuf>,
}

/// The main checkout of the project `root` lies in, and whether it is a bare
/// repository. From `root` up to the first folder with `.git`: a folder
/// `.git` makes it the main checkout; a `.git` file of a linked worktree
/// names its main; an orphan's sends the search higher; any other `.git`
/// file (a submodule's, one that cannot be read) makes its folder the main
/// checkout, and git decides from there.
pub fn resolve_main(root: &Path, home: &Path) -> Result<(PathBuf, bool), Refused> {
    let not_a_repository = || Refused::NotARepository { root: root.to_path_buf() };
    let mut found = None;
    for dir in root.ancestors() {
        let Ok(meta) = fs::symlink_metadata(dir.join(".git")) else { continue };
        if meta.is_dir() {
            found = Some(dir.to_path_buf());
            break;
        }
        match worktree::recognize(dir) {
            Some(Recognized::Linked(linked)) => {
                found = Some(linked.main.canonical().unwrap_or(linked.main));
                break;
            }
            Some(Recognized::Orphan { .. }) => {}
            None => {
                found = Some(dir.to_path_buf());
                break;
            }
        }
    }
    let main = found.ok_or_else(not_a_repository)?;
    let bare = is_bare(&main);
    // A main checkout at or above the home folder: walking it is a home scan.
    if same_folder(&main, home) || home.starts_with(&main) {
        return Err(Refused::HomeIsTheCheckout { main });
    }
    Ok((main, bare))
}

/// A bare repository: no `.git` of its own, a `HEAD` file at its top.
fn is_bare(dir: &Path) -> bool {
    fs::symlink_metadata(dir.join(".git")).is_err() && dir.join("HEAD").is_file() && dir.join("objects").is_dir()
}

fn same_folder(a: &Path, b: &Path) -> bool {
    match (crate::platform::meta::stat(a), crate::platform::meta::stat(b)) {
        (Ok(a), Ok(b)) => (a.dev, a.ino) == (b.dev, b.ino),
        _ => a == b,
    }
}

impl Scope {
    /// The project at `root`: its main checkout, then `git worktree list`
    /// through the probe contract (`git`, `tool_dirs` and `timeout` as a
    /// scan's probes run).
    pub fn resolve(
        root: &Path,
        home: &Path,
        commands: &dyn CommandRunner,
        tool_dirs: &[PathBuf],
        timeout: Duration,
    ) -> Result<Scope, Refused> {
        let root = root.canonical().map_err(|_| Refused::NotARepository { root: root.to_path_buf() })?;
        let (main, bare) = resolve_main(&root, home)?;
        let mut checkouts = if bare { Vec::new() } else { vec![main.clone()] };
        let mut missing = Vec::new();
        let listed = match git::locate(commands, tool_dirs, timeout) {
            None => Err(crate::rules::worktree::NO_GIT.to_owned()),
            Some(exe) => Git::new(commands, exe, timeout).worktree_paths(&main).known(),
        };
        let listing_error = match listed {
            Ok(paths) => {
                for path in paths {
                    // git spells the path as it wrote it: matched with the walk canonical.
                    match path.canonical() {
                        Ok(p) if p == main || checkouts.contains(&p) => {}
                        // The bare repository's own entry: its objects, never a checkout.
                        Ok(p) if is_bare(&p) => {}
                        Ok(p) if p.is_dir() => checkouts.push(p),
                        _ => missing.push(path),
                    }
                }
                None
            }
            Err(why) => Some(why),
        };
        Ok(Scope { root, main, bare, listing_error, checkouts, missing })
    }

    /// What the walk starts at: each checkout's parent, so every worktree is
    /// a region at depth 1 or more (a scan root is never one). A checkout at
    /// the filesystem's root is walked from itself.
    pub fn walk_roots(&self) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = Vec::new();
        for checkout in &self.checkouts {
            let parent = checkout.parent().unwrap_or(checkout).to_path_buf();
            if !roots.contains(&parent) {
                roots.push(parent);
            }
        }
        roots
    }

    /// The walk, pruned to the checkouts and their ancestors.
    pub fn reach(&self) -> Reach {
        Reach::subtrees(&self.checkouts)
    }

    /// The innermost checkout `path` lies in (or is), by index.
    pub fn checkout_of(&self, path: &Path) -> Option<usize> {
        self.checkouts
            .iter()
            .enumerate()
            .filter(|(_, c)| path.starts_with(c))
            .max_by_key(|(_, c)| c.components().count())
            .map(|(i, _)| i)
    }
}
