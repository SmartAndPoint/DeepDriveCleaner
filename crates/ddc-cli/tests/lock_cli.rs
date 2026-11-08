//! `ddc lock status` (story "Busy: name the scan or clean that holds the
//! lock, and offer to stop it"): it lists the live holders of the lock from
//! their records, real `deepdrivecleaner` processes held under SIGSTOP and
//! this test's own process, and never takes or probes the lock itself.

// Unix only: it stops and resumes processes.
#![cfg(unix)]

use std::fs;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime};

use ddc_core::env::Env;
use ddc_core::holders::{HOLDERS_DIR, HolderCommand, LockStatus};
use ddc_core::platform::Capability;
use ddc_core::store::{self, LockMode};
use ddc_loadgen::isolate::ClearEnv as _;
use ddc_loadgen::profiles::Profile;
use ddc_loadgen::target::Scratch;
use ddc_loadgen::{Generated, generate};

mod common;
mod contract;

/// A generated tree, large enough that a scan of it outlasts the poll for
/// its record.
struct Tree {
    _dir: Scratch,
    g: Generated,
}

impl Tree {
    fn new(entries: u64) -> Self {
        let dir = Scratch::new().unwrap();
        let g = generate(Profile::Projects, entries, 1, dir.path(), None).unwrap();
        Self { _dir: dir, g }
    }

    fn data(&self) -> &Path {
        &self.g.layout.data
    }

    fn ddc(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"));
        cmd.clear_env()
            .home_env(&self.g.home)
            .env("PATH", "")
            .env("NO_COLOR", "1")
            .args(args)
            .arg("--data-dir")
            .arg(self.data())
            .stdin(Stdio::null());
        cmd
    }

    fn status(&self) -> LockStatus {
        let out = self.ddc(&["lock", "status", "--json"]).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
        contract::holds("lock", &out.stdout)
    }

    fn env(&self) -> Env {
        Env::with_paths(&self.g.home, self.data(), &self.data().join("logs"), SystemTime::now()).unwrap()
    }

    fn records(&self) -> Vec<PathBuf> {
        let Ok(dir) = fs::read_dir(self.data().join(HOLDERS_DIR)) else { return Vec::new() };
        dir.map(|e| e.unwrap().path()).filter(|p| !p.file_name().unwrap().to_string_lossy().starts_with('.')).collect()
    }

    /// A scan of the tree, stopped by SIGSTOP once its record is there;
    /// `None` when it ended first, every one of a few tries.
    fn stopped_scan(&self) -> Option<Scan> {
        for _ in 0..5 {
            let before = self.records().len();
            let child = self
                .ddc(&["scan", "--json"])
                .arg(&self.g.layout.tree)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .unwrap();
            let mut scan = Scan(Some(child));
            let deadline = Instant::now() + Duration::from_secs(30);
            while self.records().len() == before && Instant::now() < deadline && scan.running() {
                std::thread::sleep(Duration::from_millis(1));
            }
            if scan.running() {
                scan.signal(libc::SIGSTOP);
                if scan.running() && self.records().len() > before {
                    return Some(scan);
                }
            }
            // `scan` drops here: killed, if it still runs, and reaped.
        }
        None
    }
}

/// A scan in its own process group, killed and reaped when dropped, so a
/// failed assertion leaves no stopped process behind.
struct Scan(Option<Child>);

impl Scan {
    fn pid(&self) -> i32 {
        i32::try_from(self.0.as_ref().unwrap().id()).unwrap()
    }

    fn running(&mut self) -> bool {
        self.0.as_mut().is_some_and(|c| c.try_wait().unwrap().is_none())
    }

    /// Sends `sig` to its group, only while it has not been reaped: a
    /// reaped pid may be another group's already.
    fn signal(&mut self, sig: i32) {
        if self.running() {
            let _ = ddc_core::platform::signals::signal_group(self.pid(), sig);
        }
    }

    /// Resumes it and waits for its end.
    fn finish(mut self) -> std::process::ExitStatus {
        self.signal(libc::SIGCONT);
        self.0.take().unwrap().wait().unwrap()
    }
}

impl Drop for Scan {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn text(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn two_running_scans_are_both_listed_and_the_one_left_stays_when_the_other_ends() {
    if common::unsupported("two_running_scans_are_both_listed_and_the_one_left_stays_when_the_other_ends")
        || common::lacks(
            "two_running_scans_are_both_listed_and_the_one_left_stays_when_the_other_ends",
            Capability::InUseCheck,
        )
    {
        return;
    }
    let t = Tree::new(8_000);
    assert_eq!(t.status().holders, [], "nobody holds it yet");
    let Some(a) = t.stopped_scan() else { panic!("the first scan ended before it could be stopped") };
    let Some(b) = t.stopped_scan() else { panic!("the second scan ended before it could be stopped") };
    let pids = |s: &LockStatus| s.holders.iter().map(|h| h.pid).collect::<Vec<_>>();
    let (pa, pb) = (a.pid(), b.pid());

    let both = t.status();
    assert_eq!(pids(&both), [pa, pb], "oldest first");
    for h in &both.holders {
        assert_eq!((h.command, h.mode), (HolderCommand::Scan, LockMode::Shared), "{h:?}");
        assert_eq!(h.phase, None);
    }
    let out = t.ddc(&["lock", "status"]).output().unwrap();
    assert!(text(&out).contains(&format!("pid {pa}: scan, started ")), "{}", text(&out));

    assert!(a.finish().success());
    assert_eq!(pids(&t.status()), [pb], "the other scan still holds it");
    assert!(b.finish().success());
    assert_eq!(t.status().holders, []);
    assert!(t.records().is_empty(), "each scan forgot its record");
}

#[test]
fn a_clean_that_meets_a_held_lock_names_the_holder() {
    if common::unsupported("a_clean_that_meets_a_held_lock_names_the_holder")
        || common::lacks("a_clean_that_meets_a_held_lock_names_the_holder", Capability::InUseCheck)
    {
        return;
    }
    let t = Tree::new(500);
    let _held = store::try_lock(&t.env(), LockMode::Exclusive, HolderCommand::CleanItems).unwrap();
    let pid = std::process::id();

    let out = t.ddc(&["lock", "status"]).output().unwrap();
    assert!(text(&out).starts_with(&format!("pid {pid}: clean --items, started ")), "{}", text(&out));
    assert!(text(&out).contains("re-checking its items"), "{}", text(&out));

    let mut items = ddc_loadgen::items::listed(&t.g.manifest, &t.g.layout.tree).unwrap().expect("a plan with items");
    items.items.truncate(1);
    let item = items.items[0].path.clone();
    let list = t.data().with_file_name("items.json");
    fs::write(&list, serde_json::to_vec(&items).unwrap()).unwrap();
    let out = t.ddc(&["clean", "--items", list.to_str().unwrap(), "--json"]).arg(&t.g.layout.tree).output().unwrap();
    assert!(item.exists(), "nothing was touched");
    assert_eq!(out.status.code(), Some(75), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("another ddc scan or clean is running"), "{stderr}");
    assert!(stderr.contains(&format!("\n  pid {pid}: clean --items, started ")), "{stderr}");
    let report: ddc_core::refusal::ErrorReport = contract::holds("error", &out.stdout);
    assert_eq!(report.error.code, ddc_core::refusal::ErrorCode::Busy);
    let holders = report.error.holders.unwrap();
    assert_eq!(holders.iter().map(|h| h.pid).collect::<Vec<_>>(), [i32::try_from(pid).unwrap()]);

    // The sheet's check, a dry run, shares the lock: an exclusive holder
    // blocks it, and its stream of lines gives way to the envelope.
    let out = t
        .ddc(&["clean", "--items", list.to_str().unwrap(), "--dry-run", "--json"])
        .arg(&t.g.layout.tree)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(75), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let report: ddc_core::refusal::ErrorReport = contract::holds("error", &out.stdout);
    assert_eq!(report.error.holders.map(|h| h.len()), Some(1));
}

#[test]
fn lock_status_with_nobody_holding_it_says_so_and_writes_nothing() {
    if common::unsupported("lock_status_with_nobody_holding_it_says_so_and_writes_nothing")
        || common::lacks("lock_status_with_nobody_holding_it_says_so_and_writes_nothing", Capability::InUseCheck)
    {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    fs::create_dir_all(&home).unwrap();
    let data = dir.path().join("data");
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
            .clear_env()
            .home_env(&home)
            .env("PATH", "")
            .args(args)
            .arg("--data-dir")
            .arg(&data)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let out = run(&["lock", "status"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(text(&out), "No scan or clean holds the lock.\n");
    let status: LockStatus = contract::holds("lock", &run(&["lock", "status", "--json"]).stdout);
    assert_eq!(status.holders, []);
    assert!(!data.join("lock").exists(), "no lock file was made: the lock was never taken");
}
