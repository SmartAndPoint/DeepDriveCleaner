//! `ddc clean` end to end with a temporary HOME and data directory.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime};

use ddc_core::env::Env;
use ddc_core::store::{self, LockMode};

fn ddc(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
        .env_clear()
        .env("HOME", home)
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
        let root = tmp.path().canonicalize().unwrap();
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
                out.insert(e.path(), (md.is_dir(), md.ino()));
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

#[test]
fn yes_cleans_only_safe_items_by_default() {
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
    let fx = Fx::new();
    let out = ddc(&fx.home, &["clean", "--yes", "--class", "review", "--data-dir", fx.data_arg()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("review"));
    assert!(fx.home.join("x/node_modules").exists());
}

#[test]
fn without_a_terminal_a_flag_is_required() {
    let fx = Fx::new();
    let out = ddc(&fx.home, &["clean", "--data-dir", fx.data_arg()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(fx.home.join("q/__pycache__").exists());
}

#[test]
fn clean_does_not_start_while_the_lock_is_held() {
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
