//! The lock's holder records (story "Busy: name the scan or clean that holds
//! the lock, and offer to stop it"): one record per holding process, live
//! while its pid runs as the user with its start, swept by the next holder
//! when stale, never removed by a reader.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ddc_core::env::Env;
use ddc_core::holders::{
    self, HOLDERS_DIR, Holder, HolderCommand, HolderPhase, Origin, START_TOLERANCE_US, record_name,
};
use ddc_core::platform::procs::{self, ProcStart};
use ddc_core::store::{self, LockError, LockMode};

struct Fx {
    dir: tempfile::TempDir,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("home")).unwrap();
        Self { dir }
    }

    fn env(&self) -> Env {
        let data = self.dir.path().join("data");
        Env::with_paths(&self.dir.path().join("home"), &data, &data.join("logs"), SystemTime::now()).unwrap()
    }

    fn records(&self) -> PathBuf {
        holders::dir(&self.env())
    }

    /// The file names in the records folder, sorted.
    fn names(&self) -> Vec<String> {
        let Ok(entries) = ddc_core::stats::read_dir(self.records()) else { return Vec::new() };
        let mut names: Vec<String> = entries.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    /// Writes a record of `pid` started at `start_us`, as a holder would.
    fn put(&self, pid: i32, start_us: u64, mode: LockMode) -> String {
        let h = holder(pid, start_us, mode);
        let name = record_name(pid, start_us);
        fs::create_dir_all(self.records()).unwrap();
        fs::write(self.records().join(&name), serde_json::to_vec(&h).unwrap()).unwrap();
        name
    }
}

fn holder(pid: i32, start_us: u64, mode: LockMode) -> Holder {
    Holder {
        pid,
        start_us,
        started: "2026-10-19T10:00:00Z".into(),
        command: if mode == LockMode::Shared { HolderCommand::Scan } else { HolderCommand::CleanItems },
        mode,
        origin: Origin::Terminal,
        phase: None,
        progress: None,
    }
}

const UID: u32 = 501;
const T: u64 = 1_792_404_000_000_000;

/// A probe that knows `alive`, each pid with its start.
fn probe(alive: &[(i32, u64)]) -> impl Fn(i32) -> Option<ProcStart> + '_ {
    move |pid| {
        alive.iter().find(|(p, _)| *p == pid).map(|&(_, us)| ProcStart {
            uid: UID,
            sec: us / 1_000_000,
            usec: us % 1_000_000,
        })
    }
}

fn own_pid() -> i32 {
    i32::try_from(std::process::id()).unwrap()
}

fn own_start() -> u64 {
    holders::start_us(procs::process_start(own_pid()).expect("this process has a start"))
}

#[test]
fn two_shared_records_are_both_listed_and_forgetting_one_leaves_the_other() {
    let fx = Fx::new();
    fx.put(4101, T, LockMode::Shared);
    let b = fx.put(4202, T + 5_000_000, LockMode::Shared);
    let both = [(4101, T), (4202, T + 5_000_000)];
    let listed = holders::holders_in(&fx.records(), &probe(&both), UID);
    assert_eq!(listed.iter().map(|h| h.pid).collect::<Vec<_>>(), [4101, 4202], "oldest first");
    // 4101 ended without a trace (a crash): the next holder sweeps it alone.
    let one = [(4202, T + 5_000_000)];
    holders::sweep_in(&fx.records(), &probe(&one), UID, None);
    assert_eq!(fx.names(), [b]);
    assert_eq!(holders::holders_in(&fx.records(), &probe(&one), UID).len(), 1);
}

#[test]
fn an_exclusive_record_is_listed_alone_with_its_mode() {
    let fx = Fx::new();
    fx.put(4101, T, LockMode::Exclusive);
    // The scan before it was killed and left its record.
    fx.put(4202, T - 60_000_000, LockMode::Shared);
    let listed = holders::holders_in(&fx.records(), &probe(&[(4101, T)]), UID);
    assert_eq!(listed, [holder(4101, T, LockMode::Exclusive)]);
}

#[test]
fn a_record_that_cannot_be_written_never_fails_the_lock() {
    let fx = Fx::new();
    let env = fx.env();
    fs::create_dir_all(env.data_dir()).unwrap();
    fs::write(env.data_dir().join(HOLDERS_DIR), b"a file where the folder goes").unwrap();
    let lock = store::try_lock(&env, LockMode::Exclusive, HolderCommand::Clean).expect("the lock is taken");
    assert!(holders::holders(&env).is_empty());
    holders::note_cleaning();
    holders::note_progress(1, 2);
    drop(lock);
    assert!(env.data_dir().join(HOLDERS_DIR).is_file(), "nothing was swept or replaced");
}

#[test]
fn the_sweep_removes_temp_files_of_records_that_are_not_live() {
    let fx = Fx::new();
    let live = fx.put(4101, T, LockMode::Shared);
    let kept = format!(".{live}.tmp-4101");
    let dead = format!(".{}.tmp-4202", record_name(4202, T));
    for name in [&kept, &dead] {
        fs::write(fx.records().join(name), b"{").unwrap();
    }
    holders::sweep_in(&fx.records(), &probe(&[(4101, T)]), UID, None);
    let mut want = vec![live, kept];
    want.sort();
    assert_eq!(fx.names(), want);
}

#[test]
fn a_record_is_stale_when_its_pid_is_gone_or_started_at_another_time() {
    let fx = Fx::new();
    fx.put(4101, T, LockMode::Shared); // gone
    fx.put(4202, T, LockMode::Shared); // reused: started later
    let near = fx.put(4303, T, LockMode::Shared); // boot time moved within the tolerance
    fx.put(4404, T, LockMode::Shared); // another user's process now
    let alive = [(4202, T + START_TOLERANCE_US + 1), (4303, T + START_TOLERANCE_US)];
    let start = |pid| {
        if pid == 4404 {
            return Some(ProcStart { uid: UID + 1, sec: T / 1_000_000, usec: 0 });
        }
        probe(&alive)(pid)
    };
    let listed = holders::holders_in(&fx.records(), &start, UID);
    assert_eq!(listed.iter().map(|h| h.pid).collect::<Vec<_>>(), [4303]);
    assert_eq!(fx.names().len(), 4, "a reader removes nothing");
    holders::sweep_in(&fx.records(), &start, UID, None);
    assert_eq!(fx.names(), [near], "the holder sweeps the stale ones only");
}

#[test]
fn readers_skip_temp_files_records_whose_content_differs_and_other_names() {
    let fx = Fx::new();
    let ok = fx.put(4101, T, LockMode::Shared);
    fs::write(fx.records().join(format!(".{ok}.tmp-77")), b"{").unwrap();
    fs::write(fx.records().join("notes.txt"), b"not a record").unwrap();
    // A file named for 4202 that holds 4101's record.
    fs::write(fx.records().join(record_name(4202, T)), serde_json::to_vec(&holder(4101, T, LockMode::Shared)).unwrap())
        .unwrap();
    fs::write(fx.records().join(record_name(4303, T)), b"not json").unwrap();
    let alive = [(4101, T), (4202, T), (4303, T)];
    let listed = holders::holders_in(&fx.records(), &probe(&alive), UID);
    assert_eq!(listed.iter().map(|h| h.pid).collect::<Vec<_>>(), [4101]);
    // A sweep with nobody alive leaves what is not a record's name, or a
    // record's temp file.
    fs::write(fx.records().join(".notes.tmp-1"), b"").unwrap();
    holders::sweep_in(&fx.records(), &probe(&[]), UID, None);
    assert_eq!(fx.names(), [".notes.tmp-1", "notes.txt"]);
}

#[test]
fn a_lock_writes_this_process_record_and_its_release_forgets_it() {
    let fx = Fx::new();
    let env = fx.env();
    let lock = store::try_lock(&env, LockMode::Exclusive, HolderCommand::Clean).unwrap();
    let listed = holders::holders(&env);
    assert_eq!(listed.len(), 1, "{listed:?}");
    let h = &listed[0];
    assert_eq!((h.pid, h.mode, h.command), (own_pid(), LockMode::Exclusive, HolderCommand::Clean));
    assert!(h.start_us.abs_diff(own_start()) <= START_TOLERANCE_US);
    assert_eq!(h.phase, Some(HolderPhase::Preparing), "a clean is preparing until its executor starts");
    assert_eq!(h.progress, None);
    assert_eq!(fx.names(), [record_name(h.pid, h.start_us)]);
    drop(lock);
    assert!(holders::holders(&env).is_empty());
    assert!(fx.names().is_empty(), "the record is gone with the lock");
}

#[test]
fn two_shared_locks_of_one_process_keep_the_record_until_the_last_is_released() {
    let fx = Fx::new();
    let env = fx.env();
    let a = store::try_lock(&env, LockMode::Shared, HolderCommand::Scan).unwrap();
    let b = store::try_lock(&env, LockMode::Shared, HolderCommand::Scan).unwrap();
    assert_eq!(holders::holders(&env).len(), 1, "one record per process");
    assert_eq!(holders::holders(&env)[0].phase, None, "only a clean has a phase");
    drop(a);
    assert_eq!(holders::holders(&env).len(), 1, "the other lock is still held");
    drop(b);
    assert!(fx.names().is_empty());
}

#[test]
fn a_busy_lock_names_its_live_holders() {
    let fx = Fx::new();
    let env = fx.env();
    let _clean = store::try_lock(&env, LockMode::Exclusive, HolderCommand::CleanItems).unwrap();
    match store::try_lock(&env, LockMode::Shared, HolderCommand::Scan) {
        Err(LockError::Busy { holders, .. }) => {
            assert_eq!(holders.len(), 1, "{holders:?}");
            assert_eq!((holders[0].pid, holders[0].command), (own_pid(), HolderCommand::CleanItems));
        }
        other => panic!("not busy: {other:?}"),
    }
    let message = store::try_lock(&env, LockMode::Shared, HolderCommand::Scan).unwrap_err().to_string();
    assert!(message.starts_with("another ddc scan or clean is running (lock "), "{message}");
}

#[test]
fn the_next_holder_sweeps_stale_records_and_keeps_live_ones() {
    let fx = Fx::new();
    let env = fx.env();
    let start = own_start();
    // This pid, started at another time: a reused pid's record.
    fx.put(own_pid(), start + 10 * START_TOLERANCE_US, LockMode::Exclusive);
    // A pid that cannot run.
    fx.put(i32::MAX, T, LockMode::Shared);
    // A live record: this process, within the tolerance.
    let near = fx.put(own_pid(), start + START_TOLERANCE_US / 2, LockMode::Shared);
    let lock = store::try_lock(&env, LockMode::Shared, HolderCommand::Scan).unwrap();
    let mut want = vec![near.clone(), record_name(own_pid(), start)];
    want.sort();
    assert_eq!(fx.names(), want);
    drop(lock);
    assert_eq!(fx.names(), [near], "a live record of someone else stays");
}

#[test]
fn a_record_is_written_in_the_data_dir() {
    let fx = Fx::new();
    let env = fx.env();
    let _lock = store::try_lock(&env, LockMode::Shared, HolderCommand::Scan).unwrap();
    assert_eq!(holders::dir(&env), env.data_dir().join(HOLDERS_DIR));
    assert!(Path::new(&holders::dir(&env)).is_dir());
}

#[test]
fn interrupt_refuses_a_pid_of_one_or_less() {
    for pid in [1, 0, -1, i32::MIN] {
        let e = ddc_core::platform::signals::interrupt(pid).unwrap_err();
        #[cfg(unix)]
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput, "{pid}");
        #[cfg(windows)]
        assert_eq!(e.kind(), std::io::ErrorKind::Unsupported, "{pid}");
    }
}
