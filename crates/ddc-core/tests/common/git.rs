//! Real git repositories in a test tree (story "Worktree findings and
//! provenance", DDC-003). Every call runs the `git` on `PATH` with no
//! global or system configuration, a fixed identity and no prompt, so the
//! developer's own settings (hooks, signing, a default branch) never reach a
//! fixture.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::TreeBuilder;

/// `git -C <dir> <args>` under the fixture's configuration; its stdout,
/// trimmed. Panics with git's stderr when it fails.
pub fn git(dir: &Path, args: &[&str]) -> String {
    try_git(dir, args).unwrap_or_else(|e| panic!("git {args:?} in {}: {e}", dir.display()))
}

/// [`git`], or git's stderr when it exits non-zero.
pub fn try_git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    // A git hook (the pre-push gate) exports GIT_DIR, GIT_INDEX_FILE and the
    // like; inherited, they point every fixture command at the outer repository.
    for (key, _) in std::env::vars_os() {
        if key.to_str().is_some_and(|k| k.starts_with("GIT_")) {
            cmd.env_remove(key);
        }
    }
    let out = cmd
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"])
        .args(["-c", "protocol.file.allow=always", "-c", "core.hooksPath=/dev/null"])
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// Whether a `git` runs here; `Unsupported` otherwise, for
/// [`super::skipped_without`].
pub fn available() -> std::io::Result<()> {
    match Command::new("git").arg("--version").output() {
        Ok(out) if out.status.success() => Ok(()),
        _ => Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "no git on PATH")),
    }
}

/// A repository at `rel` on branch `main` with one commit; its path.
pub fn repo(t: &TreeBuilder, rel: &str) -> PathBuf {
    let dir = t.dir(rel);
    git(&dir, &["init", "-q", "-b", "main"]);
    t.write(&format!("{rel}/README"), "hello\n");
    git(&dir, &["add", "README"]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    dir
}

/// `git worktree add <args…> <path>` from `main`; `path`.
pub fn worktree(main: &Path, path: &Path, args: &[&str]) -> PathBuf {
    let path_arg = path.to_str().expect("UTF-8 test path");
    let mut all = vec!["worktree", "add", "-q"];
    all.extend_from_slice(args);
    all.push(path_arg);
    git(main, &all);
    path.to_path_buf()
}

/// The administrative folder of the worktree at `path`, as its `.git` names it.
pub fn gitdir(path: &Path) -> PathBuf {
    PathBuf::from(git(path, &["rev-parse", "--absolute-git-dir"]))
}
