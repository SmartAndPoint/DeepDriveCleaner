//! An interrupted scan or clean leaves a consistent state (story "Coverage
//! floor and property tests for the safety invariants", DDC-005): a scan
//! killed mid-walk, a clean stopped by SIGTERM after some items, a SIGKILL
//! during one large deletion, and a temp file of the saved scan left behind.
//! After each, the data-dir lock is free, `last-scan.json` is the previous
//! file or a complete new one, a second run works and nothing outside the
//! plan was removed.
//!
//! Trees are generated in marked temporary folders, with `HOME` and
//! `--data-dir` in them. The lists hold only `delete` items: a trash item
//! would go to the real Trash whatever `HOME` is. The child runs in its own
//! process group, which the test signals with `kill(2)`.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{BufRead as _, BufReader, Read as _};
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use ddc_core::env::Env;
use ddc_core::exec::Outcome;
use ddc_core::exec::report::CleanReport;
use ddc_core::model::ScanReport;
use ddc_core::plan::{ActionKind, LISTED_SCHEMA_VERSION, ListedItem, ListedItems};
use ddc_core::rules::TOMBSTONE;
use ddc_core::store;
use ddc_loadgen::isolate::ClearEnv as _;
use ddc_loadgen::profiles::Profile;
use ddc_loadgen::target::Scratch;
use ddc_loadgen::{Generated, generate, items};

mod contract;

/// How long the child may take to reach an event on a loaded machine.
const DEADLINE: Duration = Duration::from_secs(90);

/// A generated tree, its home and data folder.
struct Tree {
    _dir: Scratch,
    g: Generated,
}

impl Tree {
    fn new(profile: Profile, entries: u64) -> Self {
        let dir = Scratch::new().unwrap();
        let g = generate(profile, entries, 1, dir.path(), None).unwrap();
        Self { _dir: dir, g }
    }

    fn tree(&self) -> &Path {
        &self.g.layout.tree
    }

    fn data(&self) -> &Path {
        &self.g.layout.data
    }

    /// `ddc <args> --data-dir <data> <tree>` with this tree's home and no `PATH`.
    fn ddc(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"));
        cmd.clear_env()
            .env("HOME", &self.g.home)
            .env("PATH", "")
            .env("NO_COLOR", "1")
            .args(args)
            .arg("--data-dir")
            .arg(self.data())
            .arg(self.tree())
            .stdin(Stdio::null());
        cmd
    }

    /// A complete scan; the saved file's bytes.
    fn scan(&self) -> Vec<u8> {
        let out = self.ddc(&["scan", "--json"]).output().unwrap();
        assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
        contract::holds::<ScanReport>("scan", &out.stdout);
        self.last_scan()
    }

    fn last_scan(&self) -> Vec<u8> {
        fs::read(self.data().join(store::LAST_SCAN)).unwrap()
    }

    /// The plan's `delete` items, written as the list `clean --items` reads.
    fn delete_list(&self) -> (PathBuf, Vec<ListedItem>) {
        let mut list = items::listed(&self.g.manifest, self.tree()).unwrap().expect("a plan with items");
        list.items.retain(|i| i.action == ActionKind::Delete);
        let path = self.g.layout.data.with_file_name("items.json");
        fs::write(&path, serde_json::to_vec(&list).unwrap()).unwrap();
        (path, list.items)
    }

    /// `clean --items` of `list` to its end; the report.
    fn clean(&self, list: &Path) -> CleanReport {
        let out = self.ddc(&["clean", "--items", list.to_str().unwrap(), "--json"]).output().unwrap();
        assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
        contract::holds("clean", &out.stdout)
    }

    /// Every path under the tree.
    fn paths(&self) -> BTreeSet<PathBuf> {
        let mut out = BTreeSet::new();
        let mut stack = vec![self.tree().to_path_buf()];
        while let Some(dir) = stack.pop() {
            for e in fs::read_dir(&dir).unwrap() {
                let e = e.unwrap();
                if e.file_type().unwrap().is_dir() {
                    stack.push(e.path());
                }
                out.insert(e.path());
            }
        }
        out
    }

    /// The consistent state every interruption must leave: the lock free,
    /// and the saved scan the `previous` bytes or a complete report.
    fn assert_consistent(&self, previous: &[u8]) {
        let lock = File::options().write(true).open(self.data().join("lock")).unwrap();
        lock.try_lock().expect("the data-dir lock is free");
        drop(lock);
        let saved = self.last_scan();
        if saved != previous {
            contract::holds::<ScanReport>("scan", &saved);
        }
    }
}

/// A child in its own process group, its stderr read line by line as it comes.
struct Watched {
    child: Child,
    group: i32,
    lines: mpsc::Receiver<String>,
    seen: Vec<String>,
    stdout: Option<JoinHandle<Vec<u8>>>,
    reaped: bool,
}

impl Watched {
    fn spawn(mut cmd: Command) -> Self {
        let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0).spawn().unwrap();
        let group = i32::try_from(child.id()).unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let stdout = std::thread::spawn(move || {
            let mut buf = Vec::new();
            stdout.read_to_end(&mut buf).unwrap();
            buf
        });
        let stderr = child.stderr.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                if tx.send(line.unwrap()).is_err() {
                    break;
                }
            }
        });
        Self { child, group, lines, seen: Vec::new(), stdout: Some(stdout), reaped: false }
    }

    /// Reads stderr until `count` lines match `pred`.
    fn wait_for(&mut self, what: &str, count: usize, pred: impl Fn(&serde_json::Value) -> bool) {
        let deadline = Instant::now() + DEADLINE;
        let mut matched = self.events().iter().filter(|e| pred(e)).count();
        while matched < count {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .unwrap_or_else(|e| panic!("{what}: {e} after {} lines:\n{}", self.seen.len(), self.seen.join("\n")));
            if event(&line).is_some_and(|e| pred(&e)) {
                matched += 1;
            }
            self.seen.push(line);
        }
    }

    /// The events read so far.
    fn events(&self) -> Vec<serde_json::Value> {
        self.seen.iter().filter_map(|l| event(l)).collect()
    }

    fn signal(&self, sig: i32) {
        kill(self.group, sig).unwrap_or_else(|e| panic!("kill(-{}, {sig}): {e}", self.group));
    }

    /// Stops the group; `false` when the child already ended, so there is
    /// nothing left to interrupt (macOS refuses to signal a group that holds
    /// only a zombie).
    fn stop(&mut self) -> bool {
        match kill(self.group, libc::SIGSTOP) {
            Ok(()) => true,
            Err(e) => {
                assert!(self.child.try_wait().unwrap().is_some(), "kill(-{}, SIGSTOP): {e}", self.group);
                false
            }
        }
    }

    /// Waits for the end; the status, stdout and every event.
    fn finish(mut self) -> (ExitStatus, Vec<u8>, Vec<serde_json::Value>) {
        let status = self.child.wait().unwrap();
        self.reaped = true;
        self.seen.extend(self.lines.iter());
        let stdout = self.stdout.take().unwrap().join().unwrap();
        (status, stdout, self.events())
    }
}

impl Drop for Watched {
    /// A failed test never leaves a stopped child behind.
    fn drop(&mut self) {
        if !self.reaped {
            let _ = kill(self.group, libc::SIGKILL);
            let _ = self.child.wait();
        }
    }
}

/// Sends `sig` to the process group `group`.
#[allow(unsafe_code)]
fn kill(group: i32, sig: i32) -> std::io::Result<()> {
    // SAFETY: kill(2) takes no pointers; a negative pid names a process group.
    let sent = unsafe { libc::kill(-group, sig) };
    if sent == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

/// A progress event, or `None` for the CLI's own lines (`ddc: …`).
fn event(line: &str) -> Option<serde_json::Value> {
    line.starts_with('{').then(|| serde_json::from_str(line).unwrap())
}

/// Stopped first, so the run cannot move on before the signal: SIGSTOP,
/// `sig`, SIGCONT. `false` when the run had already ended. SIGKILL ends a
/// stopped process at once, and macOS then refuses SIGCONT to the group of
/// a zombie, so it gets no SIGCONT.
fn stop_and_signal(w: &mut Watched, sig: i32) -> bool {
    if !w.stop() {
        return false;
    }
    w.signal(sig);
    if sig != libc::SIGKILL {
        w.signal(libc::SIGCONT);
    }
    true
}

/// `.last-scan.json.tmp-*` files in `data`.
fn temp_files(data: &Path) -> Vec<String> {
    fs::read_dir(data)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(".last-scan.json.tmp-"))
        .collect()
}

#[test]
fn a_scan_killed_mid_walk_leaves_the_saved_scan_whole_and_the_lock_free() {
    // A fast machine can walk the whole tree before the first tick is read
    // and the scan end before the stop: such a run interrupted nothing, so it
    // is tried again, up to `ATTEMPTS` times.
    const ATTEMPTS: usize = 5;
    let t = Tree::new(Profile::Projects, 8_000);
    for sig in [libc::SIGTERM, libc::SIGKILL] {
        let mut interrupted = false;
        for _ in 0..ATTEMPTS {
            let previous = t.scan();
            let mut w = Watched::spawn(t.ddc(&["scan", "--json", "--progress", "json"]));
            w.wait_for("the first walk tick", 1, |e| e["event"] == "walk");

            interrupted = stop_and_signal(&mut w, sig);
            let (status, _, _) = w.finish();

            if interrupted {
                assert_eq!(status.signal(), Some(sig), "a scan has no handler: the signal ends it");
            } else {
                assert!(status.success(), "the scan ended before the stop: {status}");
            }
            t.assert_consistent(&previous);
            let again = t.scan();
            t.assert_consistent(&again);
            if interrupted {
                break;
            }
        }
        assert!(interrupted, "signal {sig}: the scan ended before the stop in {ATTEMPTS} of {ATTEMPTS} runs");
    }
}

#[test]
fn a_clean_stopped_after_its_kth_item_ends_the_current_one_and_runs_no_other() {
    const K: usize = 3;
    let t = Tree::new(Profile::Projects, 2_000);
    let previous = t.scan();
    let (list, items) = t.delete_list();
    let n = items.len();
    assert!(n > 10 * K, "enough items that the clean cannot finish before the signal: {n}");
    let before = t.paths();
    let mut w = Watched::spawn(t.ddc(&["clean", "--items", list.to_str().unwrap(), "--json", "--progress", "json"]));
    w.wait_for("the k-th item event", K, |e| e["event"] == "item");

    assert!(stop_and_signal(&mut w, libc::SIGTERM), "the clean ended before the stop");
    let (status, stdout, events) = w.finish();

    assert_eq!(status.signal(), Some(libc::SIGTERM), "the clean ends by the signal it got");
    // Each item's event follows its outcome, and the stop comes some time
    // after the k-th is read: at least k items ran, then only `not-run`.
    let outcomes: Vec<&str> =
        events.iter().filter(|e| e["event"] == "item").map(|e| e["outcome"].as_str().unwrap()).collect();
    assert_eq!(outcomes.len(), n, "an event per item, not-run ones too");
    let done = outcomes.iter().take_while(|o| **o != "not-run").count();
    assert!(done >= K && done < n, "{done} of {n} done: {outcomes:?}");
    assert!(outcomes[done..].iter().all(|o| *o == "not-run"), "{outcomes:?}");
    let r: CleanReport = contract::holds("clean", &stdout);
    assert!(r.cancelled, "the report says the run was stopped");
    assert_eq!(r.items.iter().filter(|i| i.outcome == Outcome::NotRun).count(), n - done);
    for i in &r.items {
        match i.outcome {
            Outcome::Deleted => assert!(!i.path.exists(), "{:?} deleted", i.path),
            Outcome::NotRun => assert!(i.path.exists(), "{:?} not run", i.path),
            ref other => panic!("{:?} ended {other:?}", i.path),
        }
    }
    let deleted: Vec<&Path> =
        r.items.iter().filter(|i| i.outcome == Outcome::Deleted).map(|i| i.path.as_path()).collect();
    let after = t.paths();
    for gone in before.difference(&after) {
        assert!(deleted.iter().any(|d| gone.starts_with(d)), "{gone:?} removed outside the deleted items");
    }
    let new: Vec<&PathBuf> = after.difference(&before).collect();
    assert!(new.is_empty(), "the current item ran to its end, no tombstone: {new:?}");
    assert!(r.last_scan_saved);
    t.assert_consistent(&previous);

    let second = t.clean(&list);
    assert!(!second.cancelled);
    for i in r.items.iter().filter(|i| i.outcome == Outcome::NotRun) {
        let again = second.items.iter().find(|j| j.path == i.path).unwrap();
        assert_eq!(again.outcome, Outcome::Deleted, "{:?} cleaned by the second run", i.path);
    }
    t.assert_consistent(&previous);
}

#[test]
fn a_sigkill_during_a_large_deletion_leaves_the_item_whole_gone_or_a_tombstone() {
    // About 3 000 files in one flat `node_modules`, beside a flat folder outside any finding.
    let t = Tree::new(Profile::Wide, 6_000);
    let previous = t.scan();
    let (list, items) = t.delete_list();
    let [item] = items.as_slice() else { panic!("one delete item: {items:?}") };
    let before = t.paths();
    let mut w = Watched::spawn(t.ddc(&["clean", "--items", list.to_str().unwrap(), "--json", "--progress", "json"]));
    w.wait_for("the cleaning phase", 1, |e| e["phase"] == "cleaning");
    // The rename to the tombstone is the deletion's first step.
    let deadline = Instant::now() + DEADLINE;
    while item.path.exists() {
        assert!(Instant::now() < deadline, "the deletion never started");
        std::thread::yield_now();
    }

    w.signal(libc::SIGKILL);
    let (status, _, _) = w.finish();

    assert_eq!(status.signal(), Some(libc::SIGKILL));
    let parent = item.path.parent().unwrap();
    let tombs: Vec<PathBuf> = fs::read_dir(parent)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_string_lossy().contains(".ddc-deleting-"))
        .collect();
    assert!(tombs.len() <= 1, "{tombs:?}");
    let after = t.paths();
    for gone in before.difference(&after) {
        assert!(gone.starts_with(&item.path), "{gone:?} removed outside the item");
    }
    for new in after.difference(&before) {
        assert!(tombs.iter().any(|tomb| new.starts_with(tomb)), "{new:?} appeared");
    }
    t.assert_consistent(&previous);

    let rescan: ScanReport = serde_json::from_slice(&t.scan()).unwrap();
    if let Some(tomb) = tombs.first() {
        let f = rescan.findings.iter().find(|f| &f.path == tomb).expect("the tombstone is a finding");
        assert_eq!(f.rule, TOMBSTONE);
        let list = ListedItems {
            schema_version: LISTED_SCHEMA_VERSION,
            items: vec![ListedItem {
                path: f.path.clone(),
                dev: f.dev,
                ino: f.ino,
                rule: f.rule.clone(),
                class: f.class,
                action: ActionKind::Delete,
                permanent: false,
            }],
        };
        let path = t.data().with_file_name("tomb.json");
        fs::write(&path, serde_json::to_vec(&list).unwrap()).unwrap();
        let r = t.clean(&path);
        assert_eq!(r.items[0].outcome, Outcome::Deleted, "the next clean removes the tombstone");
        assert!(!tomb.exists());
    } else if item.path.exists() {
        assert!(rescan.findings.iter().any(|f| f.path == item.path), "a whole item is found again");
    }
    t.assert_consistent(&t.last_scan());
}

/// `store::write_atomic` writes `.last-scan.json.tmp-<pid>`, then renames it:
/// a SIGKILL between the two leaves the temp file. It is allowed to stay.
/// Scans share the lock, so a sweep under it could remove another scan's
/// temp file before its rename and fail that scan; nothing reads it, and a
/// later run with the same pid truncates it.
#[test]
fn a_temp_file_left_by_a_kill_is_ignored_and_the_next_save_replaces_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let (home, data) = (root.join("home"), root.join("data"));
    fs::create_dir_all(home.join(".npm/_cacache")).unwrap();
    fs::write(home.join(".npm/_cacache/blob"), [1u8; 9_000]).unwrap();
    let scan = || {
        let out = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"))
            .clear_env()
            .env("HOME", &home)
            .env("PATH", "")
            .args(["scan", "--json", "--data-dir", data.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    };
    scan();
    let saved = fs::read(data.join(store::LAST_SCAN)).unwrap();
    // As a kill mid-write leaves it: the start of a report.
    let stale = data.join(".last-scan.json.tmp-1");
    fs::write(&stale, &saved[..saved.len() / 2]).unwrap();

    let env = Env::with_paths(&home, &data, &data.join("logs"), SystemTime::now()).unwrap();
    let read = store::read_last_scan(&env).unwrap().expect("the saved report");
    assert_eq!(serde_json::to_vec_pretty(&read).unwrap(), saved, "the reader opens only last-scan.json");
    let mut held = File::open(data.join(store::LAST_SCAN)).unwrap();
    fs::write(home.join(".npm/_cacache/more"), [2u8; 9_000]).unwrap();
    scan();

    let again = fs::read(data.join(store::LAST_SCAN)).unwrap();
    contract::holds::<ScanReport>("scan", &again);
    let mut old = Vec::new();
    held.read_to_end(&mut old).unwrap();
    assert!(old == saved, "replaced by a rename: a reader of the old file still reads it whole");
    assert_eq!(temp_files(&data), [".last-scan.json.tmp-1"], "allowed to stay: nothing sweeps it");
    assert!(fs::read(&stale).unwrap() == saved[..saved.len() / 2], "left as it was");
}
