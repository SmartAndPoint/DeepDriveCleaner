//! Project activity: when was a project last touched? Directory mtimes are
//! useless (any new file changes them), so only explicit signals count.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Files whose mtime marks activity in a project root.
const SIGNALS: &[&str] = &[
    "package.json",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
    "pyproject.toml",
    "uv.lock",
    "poetry.lock",
    "Pipfile",
    "Pipfile.lock",
    "setup.py",
    "setup.cfg",
    "Cargo.toml",
    "Cargo.lock",
    "go.mod",
    "go.sum",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "Podfile",
    "Podfile.lock",
    "CMakeLists.txt",
    "Gemfile",
    "Gemfile.lock",
    "composer.json",
];

/// Latest activity in `root`: git index/HEAD (following a worktree's
/// `gitdir:`), manifests, lock files and `requirements*.txt`. `None` = no signal.
pub fn last_activity(root: &Path) -> Option<SystemTime> {
    let mut candidates: Vec<PathBuf> = SIGNALS.iter().map(|s| root.join(s)).collect();
    if let Some(gitdir) = git_dir(root) {
        candidates.push(gitdir.join("index"));
        candidates.push(gitdir.join("HEAD"));
    }
    if let Ok(rd) = crate::stats::read_dir(root) {
        candidates.extend(rd.filter_map(Result::ok).map(|e| e.path()).filter(|p| {
            p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("requirements") && n.ends_with(".txt"))
        }));
    }
    candidates.iter().filter_map(|p| fs::symlink_metadata(p).and_then(|m| m.modified()).ok()).max()
}

/// The git directory of a work tree: `.git` itself, or the target of the
/// `gitdir:` line in a worktree's `.git` file.
pub fn git_dir(root: &Path) -> Option<PathBuf> {
    let dot_git = root.join(".git");
    let md = fs::symlink_metadata(&dot_git).ok()?;
    if md.is_dir() {
        return Some(dot_git);
    }
    let text = fs::read_to_string(&dot_git).ok()?;
    let target = text.lines().find_map(|l| l.strip_prefix("gitdir:"))?.trim();
    let target = Path::new(target);
    Some(if target.is_absolute() { target.to_path_buf() } else { root.join(target) })
}
