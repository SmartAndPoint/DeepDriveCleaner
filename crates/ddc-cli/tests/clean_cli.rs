//! `ddc clean` end to end with a temporary HOME and data directory.

use ddc_core::platform::Canonical as _;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime};

use ddc_core::env::Env;
use ddc_core::store::{self, LockMode};
use ddc_loadgen::isolate::ClearEnv as _;

mod common;

fn ddc(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .clear_env()
        .home_env(home)
        .env("PATH", "")
        .env("NO_COLOR", "1")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

struct Fx {
    tmp: tempfile::TempDir,
    home: PathBuf,
    data: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonical().unwrap();
        let (home, data) = (root.join("home"), root.join("data"));
        let fx = Self { tmp, home, data };
        let old = SystemTime::now() - Duration::from_secs(90 * 86_400);
        fx.write("p/package.json", b"{}");
        fx.write("p/package-lock.json", b"{}");
        for f in ["p/package.json", "p/package-lock.json"] {
            File::options().write(true).open(fx.home.join(f)).unwrap().set_modified(old).unwrap();
        }
        fx.write("p/node_modules/dep/index.js", &[1u8; 30_000]);
        fx.write("q/__pycache__/m.cpython-312.pyc", &[2u8; 10_000]);
        fx.write("x/node_modules/orphan.js", &[3u8; 5_000]);
        fx
    }

    fn write(&self, rel: &str, content: &[u8]) {
        let p = self.home.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    fn data_arg(&self) -> &str {
        self.data.to_str().unwrap()
    }

    /// path → (kind, inode) for everything under HOME.
    fn snapshot(&self) -> BTreeMap<PathBuf, (bool, u64)> {
        let mut out = BTreeMap::new();
        let mut stack = vec![self.home.clone()];
        while let Some(dir) = stack.pop() {
            for e in fs::read_dir(&dir).unwrap() {
                let e = e.unwrap();
                let md = fs::symlink_metadata(e.path()).unwrap();
                out.insert(e.path(), (md.is_dir(), common::id(e.path()).1));
                if md.is_dir() {
                    stack.push(e.path());
                }
            }
        }
        out
    }
}

#[test]
fn dry_run_changes_nothing_and_writes_no_journal() {
    if common::unsupported("dry_run_changes_nothing_and_writes_no_journal") {
        return;
    }
    let fx = Fx::new();
    let before = fx.snapshot();

    let out = ddc(&fx.home, &["clean", "--dry-run", "--data-dir", fx.data_arg()]);

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("Cleanup plan:") && text.contains("Dry run: nothing was changed."), "{text}");
    assert_eq!(fx.snapshot(), before, "tree identical: paths, kinds, inodes");
    assert!(!fx.data.join("logs/actions.jsonl").exists());
    let _ = &fx.tmp;
}

/// `clean` takes `--worktree-min-age` as `scan` does: a clean Claude Code
/// worktree made a moment ago stays under the default two days, and with
/// `--worktree-min-age 0` a person's `clean --yes --class rebuildable`
/// removes it through git, its branch kept.
#[test]
fn clean_yes_with_worktree_min_age_zero_removes_a_fresh_agent_worktree() {
    if common::unsupported("clean_yes_with_worktree_min_age_zero_removes_a_fresh_agent_worktree") {
        return;
    }
    let fx = Fx::new();
    let main = fx.home.join("proj");
    fs::create_dir_all(&main).unwrap();
    let made = common::git(&main, &["init", "-q", "-b", "main"]);
    let Some(_) = common::skipped_without("clean_yes_with_worktree_min_age_zero_removes_a_fresh_agent_worktree", made)
    else {
        return;
    };
    fx.write("proj/README", b"hello\n");
    common::git(&main, &["add", "README"]).unwrap();
    common::git(&main, &["commit", "-q", "-m", "init"]).unwrap();
    let wt = main.join(".claude/worktrees/x");
    common::git(&main, &["worktree", "add", "-q", "-b", "worktree-x", wt.to_str().unwrap()]).unwrap();
    let yes = ["clean", "--yes", "--class", "rebuildable", "--data-dir", fx.data_arg()];

    let out = ddc(&fx.home, &yes);

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(wt.join("README").exists(), "within the two-day window it stays");

    let zero: Vec<&str> = yes.iter().copied().chain(["--worktree-min-age", "0"]).collect();
    let out = ddc(&fx.home, &zero);

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(!wt.exists(), "removed: {}", String::from_utf8_lossy(&out.stdout));
    let listed = common::git(&main, &["worktree", "list", "--porcelain"]).unwrap();
    assert!(!listed.contains(".claude/worktrees/x"), "{listed}");
    assert!(!main.join(".git/worktrees/x").exists(), "its administrative folder went with it");
    assert_eq!(common::git(&main, &["branch", "--list", "worktree-x"]).unwrap(), "worktree-x", "the branch is kept");
    let journal = fs::read_to_string(fx.data.join("logs/actions.jsonl")).unwrap();
    let line: serde_json::Value = serde_json::from_str(journal.lines().next_back().unwrap()).unwrap();
    assert_eq!(
        (&line["rule"], &line["method"], &line["outcome"]),
        (&"git.worktree".into(), &"native".into(), &"native".into())
    );
    let args: Vec<&str> = line["commands"][0]["args"].as_array().unwrap().iter().map(|a| a.as_str().unwrap()).collect();
    assert!(args.ends_with(&["worktree", "remove", wt.to_str().unwrap()]), "{args:?}");
    assert!(!args.contains(&"--force") && !args.contains(&"prune"), "{args:?}");
}

#[test]
fn yes_cleans_only_safe_items_by_default() {
    if common::unsupported("yes_cleans_only_safe_items_by_default") {
        return;
    }
    let fx = Fx::new();

    let out = ddc(&fx.home, &["clean", "--yes", "--data-dir", fx.data_arg()]);

    assert!(
        out.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(!fx.home.join("q/__pycache__").exists(), "safe item deleted");
    assert!(fx.home.join("p/node_modules").exists(), "rebuildable needs --class rebuildable");
    assert!(fx.home.join("x/node_modules").exists(), "review never under --yes");
    let journal = fs::read_to_string(fx.data.join("logs/actions.jsonl")).unwrap();
    assert_eq!(journal.lines().count(), 1, "{journal}");
    assert!(journal.contains("\"rule\":\"python.pycache\"") && journal.contains("\"outcome\":\"deleted\""));
    // Story "Progress output while ddc clean runs": the line is for terminals only.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains('\r') && !stderr.contains("\x1b[2K"), "{stderr:?}");
    assert!(stderr.lines().all(|l| l.is_empty() || l.starts_with("ddc: warning:")), "{stderr:?}");
}

#[test]
fn yes_with_rebuildable_class_cleans_idle_projects() {
    if common::unsupported("yes_with_rebuildable_class_cleans_idle_projects") {
        return;
    }
    let fx = Fx::new();

    let out =
        ddc(&fx.home, &["clean", "--yes", "--class", "safe", "--class", "rebuildable", "--data-dir", fx.data_arg()]);

    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(!fx.home.join("p/node_modules").exists());
    assert!(fx.home.join("p/package.json").exists());
    assert!(fx.home.join("x/node_modules").exists());
}

#[test]
fn yes_with_review_class_is_rejected() {
    if common::unsupported("yes_with_review_class_is_rejected") {
        return;
    }
    let fx = Fx::new();
    let out = ddc(&fx.home, &["clean", "--yes", "--class", "review", "--data-dir", fx.data_arg()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("review"));
    assert!(fx.home.join("x/node_modules").exists());
}

#[test]
fn without_a_terminal_a_flag_is_required() {
    if common::unsupported("without_a_terminal_a_flag_is_required") {
        return;
    }
    let fx = Fx::new();
    let out = ddc(&fx.home, &["clean", "--data-dir", fx.data_arg()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(fx.home.join("q/__pycache__").exists());
}

#[test]
fn clean_does_not_start_while_the_lock_is_held() {
    if common::unsupported("clean_does_not_start_while_the_lock_is_held") {
        return;
    }
    let fx = Fx::new();
    let env = Env::with_paths(&fx.home, &fx.data, &fx.data.join("logs"), SystemTime::now()).unwrap();
    let _held = store::try_lock(&env, LockMode::Shared).unwrap();

    let out = ddc(&fx.home, &["clean", "--yes", "--data-dir", fx.data_arg()]);

    assert_eq!(out.status.code(), Some(75), "a held lock exits 75 (EX_TEMPFAIL)");
    assert!(String::from_utf8_lossy(&out.stderr).contains("another ddc scan or clean is running"));
    assert!(fx.home.join("q/__pycache__").exists());
}

#[test]
fn scan_does_not_start_while_a_clean_holds_the_lock() {
    if common::unsupported("scan_does_not_start_while_a_clean_holds_the_lock") {
        return;
    }
    let fx = Fx::new();
    let env = Env::with_paths(&fx.home, &fx.data, &fx.data.join("logs"), SystemTime::now()).unwrap();
    // A clean holds the lock exclusively; scans share it with each other.
    let _held = store::try_lock(&env, LockMode::Exclusive).unwrap();

    let out = ddc(&fx.home, &["scan", "--json", "--data-dir", fx.data_arg()]);

    assert_eq!(out.status.code(), Some(75));
    assert!(out.stdout.is_empty());
    assert!(!fx.data.join("last-scan.json").exists());
}

/// Story "Faster clean: re-check only what the selection needs", AC 8: the
/// terminal's clean keeps its full rescan before its plan; only a clean of
/// an explicit list is pruned.
#[test]
fn the_terminal_clean_still_rescans_the_whole_home() {
    if common::unsupported("the_terminal_clean_still_rescans_the_whole_home") {
        return;
    }
    let fx = Fx::new();
    for i in 0..8 {
        fx.write(&format!("big/t{i}/deep/f.bin"), b"x");
    }
    let scan = ddc(&fx.home, &["scan", "--json", "--data-dir", fx.data_arg()]);
    assert!(scan.status.success(), "stderr: {}", String::from_utf8_lossy(&scan.stderr));
    let whole = serde_json::from_slice::<serde_json::Value>(&scan.stdout).unwrap()["totals"]["files"].clone();

    let out = ddc(&fx.home, &["clean", "--yes", "--progress", "json", "--data-dir", fx.data_arg()]);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {stderr}");
    let recheck = stderr
        .lines()
        .filter(|l| l.starts_with('{'))
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .rfind(|e| e["event"] == "walk" && e["step"] == "recheck")
        .expect("a recheck tick");
    assert_eq!(recheck["files"], whole, "the whole home, unrelated trees included");
    assert!(!fx.home.join("q/__pycache__").exists());
}

/// A sparse file (a VM disk, a database) counts at its allocated size, never
/// its apparent one: 4 KiB of data in a 1 GiB file is 4 KiB in `scan`, in
/// `ddc du` and in the clean's journal (safety ADR, amendment of 2026-11-04).
/// Unix: a Windows `set_len` allocates.
#[cfg(unix)]
#[test]
fn a_sparse_file_counts_its_allocated_size_in_scan_du_and_clean() {
    use std::io::{Seek as _, SeekFrom, Write as _};
    if common::unsupported("a_sparse_file_counts_its_allocated_size_in_scan_du_and_clean") {
        return;
    }
    let fx = Fx::new();
    let nm = fx.home.join("p/node_modules");
    let mut f = File::create(nm.join("disk.img")).unwrap();
    f.set_len(1 << 30).unwrap();
    f.seek(SeekFrom::Start(512 << 20)).unwrap();
    f.write_all(&[9u8; 4096]).unwrap();
    drop(f);
    let alloc = |p: &Path| std::os::unix::fs::MetadataExt::blocks(&fs::metadata(p).unwrap()) * 512;
    let sparse = alloc(&nm.join("disk.img"));
    assert!(sparse < 1 << 20, "test precondition: the filesystem keeps the hole: {sparse}");

    let out = ddc(&fx.home, &["scan", "--json", "--data-dir", fx.data_arg()]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let finding = report["findings"].as_array().unwrap().iter().find(|f| f["path"] == nm.to_str().unwrap()).unwrap();
    let bytes = finding["bytes"].as_u64().unwrap();
    assert!(bytes < 1 << 20, "scan: {bytes}");

    let du = ddc(&fx.home, &["du", nm.to_str().unwrap()]);
    assert!(du.status.success(), "stderr: {}", String::from_utf8_lossy(&du.stderr));
    let kib: u64 = String::from_utf8(du.stdout).unwrap().split('\t').next().unwrap().parse().unwrap();
    assert!(kib < 1024, "du: {kib} KiB");

    let out =
        ddc(&fx.home, &["clean", "--yes", "--class", "safe", "--class", "rebuildable", "--data-dir", fx.data_arg()]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(!nm.exists());
    let journal = fs::read_to_string(fx.data.join("logs/actions.jsonl")).unwrap();
    let line: serde_json::Value = journal
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .find(|l: &serde_json::Value| l["path"] == nm.to_str().unwrap())
        .unwrap();
    assert_eq!(line["bytes"].as_u64().unwrap(), bytes, "the clean counts what the scan counted");
}
