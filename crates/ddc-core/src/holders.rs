//! Who holds the data-dir lock: one record per holding process, so a busy
//! command and `ddc lock status` can name the holders (story "Busy: name the
//! scan or clean that holds the lock, and offer to stop it"; ADR "Menu bar
//! app: Swift shell over the ddc CLI", §6 item 16).
//!
//! A holder writes `<data-dir>/lock.holders/<pid>-<start_us>.json` right
//! after [`crate::store::try_lock`] took the lock, and forgets it before the
//! lock is released ([`release`], from the lock's `Drop`) or before the
//! process ends by a signal ([`end_by_signal`]: `reraise` skips `Drop`).
//!
//! A record is **live** while its pid runs as the caller's user and that
//! pid's start time is within [`START_TOLERANCE_US`] of the one in its name.
//! Any other record is stale: a crash or a SIGKILL leaves one. Readers
//! ignore stale records and never remove anything; only a holder, after it
//! took the lock, sweeps them. The executable is not part of liveness, so a
//! holder updated in place (`brew upgrade`, Sparkle) keeps its record.
//!
//! The records are advisory: a record that cannot be written never fails
//! the command, and a busy command whose holder wrote none (a ddc older than
//! this module, or the moment between the lock and the record) names none.

use std::fs;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::env::Env;
use crate::platform::procs::{self, ProcStart};
use crate::platform::signals;
use crate::store::LockMode;

/// The folder of the records, in the data dir.
pub const HOLDERS_DIR: &str = "lock.holders";

/// How far a pid's start time may be from a record's and still be its
/// process: Linux reads it as boot time plus ticks, and boot time can move
/// by a second between two reads.
pub const START_TOLERANCE_US: u64 = 2_000_000;

/// The least time between two progress writes of one record: each write
/// fsyncs, and a clean can have thousands of items.
const PROGRESS_GAP: Duration = Duration::from_secs(1);

/// Version of [`LockStatus`] (`docs/lock.schema.json`).
pub const LOCK_SCHEMA_VERSION: u32 = 1;

/// `ddc lock status --json`: the live holders of the lock, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LockStatus {
    pub schema_version: u32,
    pub holders: Vec<Holder>,
}

/// One holder of the lock, as its record holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Holder {
    pub pid: i32,
    /// The process's start, in microseconds since the epoch: the number in
    /// the record's name.
    pub start_us: u64,
    /// The same start, RFC 3339 in UTC, to the second.
    pub started: String,
    pub command: HolderCommand,
    pub mode: LockMode,
    pub origin: Origin,
    /// A clean's phase; absent for every other command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<HolderPhase>,
    /// A clean's items done of its total, once its first item ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<HolderProgress>,
}

/// What took the lock: one value per caller of `try_lock`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum HolderCommand {
    Scan,
    BackgroundScan,
    Clean,
    CleanDryRun,
    CleanItems,
    CleanItemsDryRun,
    DiskFree,
    DiskFreeDryRun,
    KeepRemove,
}

/// Where the holder was started. A guess, for the words only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    /// The binary runs from an app bundle's `Contents/Helpers/`, without a
    /// terminal.
    App,
    /// stdin or stderr is a terminal.
    Terminal,
    /// Anything else: a script, a launch agent, a pipe.
    Other,
}

/// A clean's phase: what a SIGINT does to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum HolderPhase {
    /// Re-checking the items: no handler yet, a SIGINT ends it at once and
    /// nothing has been removed.
    Preparing,
    /// The executor runs: a SIGINT stops it after its current item.
    Cleaning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HolderProgress {
    pub done: u64,
    pub total: u64,
}

/// A record of this process, in one data dir.
struct Ours {
    /// The records folder: the key.
    dir: PathBuf,
    path: PathBuf,
    holder: Holder,
    /// The locks this process holds in `dir`: two shared ones in a test.
    count: u32,
    /// When the record was last written.
    written: Instant,
    /// A progress change held back by the gap, not written yet.
    dirty: bool,
}

/// This process's records, one per data dir it holds the lock in, and
/// whether a flusher thread writes their held-back progress.
struct State {
    records: Vec<Ours>,
    flushing: bool,
}

static STATE: Mutex<State> = Mutex::new(State { records: Vec::new(), flushing: false });

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The records folder of `env`.
pub fn dir(env: &Env) -> PathBuf {
    env.data_dir().join(HOLDERS_DIR)
}

/// The file name of the record of `pid` started at `start_us`.
pub fn record_name(pid: i32, start_us: u64) -> String {
    format!("{pid}-{start_us}.json")
}

/// The pid and start of a record's file name; `None` for any other name.
fn parse_name(name: &str) -> Option<(i32, u64)> {
    let (pid, start) = name.strip_suffix(".json")?.split_once('-')?;
    Some((pid.parse().ok()?, start.parse().ok()?))
}

/// A start in microseconds since the epoch.
pub fn start_us(p: ProcStart) -> u64 {
    p.sec.saturating_mul(1_000_000).saturating_add(p.usec)
}

/// Whether the record of `pid` started at `start_us` is live, given what
/// the probe read of `pid` now (`None`: no such process).
pub fn live(start_us_of_record: u64, now: Option<ProcStart>, uid: u32) -> bool {
    now.is_some_and(|p| p.uid == uid && start_us(p).abs_diff(start_us_of_record) <= START_TOLERANCE_US)
}

/// The live holders of `env`'s lock, oldest first.
pub fn holders(env: &Env) -> Vec<Holder> {
    holders_in(&dir(env), &procs::process_start, procs::uid())
}

/// The live holders whose records are in `dir`, oldest first, `start`
/// reading a pid's start and owner. A record whose content does not match
/// its name, or does not read, is skipped. Removes nothing.
pub fn holders_in(dir: &Path, start: &dyn Fn(i32) -> Option<ProcStart>, uid: u32) -> Vec<Holder> {
    let mut out: Vec<Holder> = records(dir)
        .into_iter()
        .filter(|(_, (pid, us))| live(*us, start(*pid), uid))
        .filter_map(|(path, key)| {
            let holder: Holder = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
            ((holder.pid, holder.start_us) == key).then_some(holder)
        })
        .collect();
    out.sort_by_key(|h| (h.start_us, h.pid));
    out
}

/// Removes the records in `dir` that are not live, but `keep`, and the
/// temp files a killed write of a record that is not live left. Called by a
/// holder only, after it took the lock.
pub fn sweep_in(dir: &Path, start: &dyn Fn(i32) -> Option<ProcStart>, uid: u32, keep: Option<&Path>) {
    for (path, (pid, us)) in records(dir) {
        if Some(path.as_path()) != keep && !live(us, start(pid), uid) {
            forget_file(&path);
        }
    }
    let Ok(entries) = crate::stats::read_dir(dir) else { return };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some((pid, us)) = name.to_str().and_then(temp_of) else { continue };
        if !live(us, start(pid), uid) {
            forget_file(&e.path());
        }
    }
}

/// The record a `write_atomic` temp file `.<record>.tmp-<pid>` was for.
fn temp_of(name: &str) -> Option<(i32, u64)> {
    let (record, writer) = name.strip_prefix('.')?.rsplit_once(".tmp-")?;
    writer.parse::<u32>().ok()?;
    parse_name(record)
}

/// The records in `dir` by their names: dot-files (`write_atomic`'s temp
/// files) and other names are left out.
fn records(dir: &Path) -> Vec<(PathBuf, (i32, u64))> {
    let Ok(entries) = crate::stats::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let name = name.to_str()?;
            if name.starts_with('.') {
                return None;
            }
            Some((e.path(), parse_name(name)?))
        })
        .collect()
}

/// Writes this process's record in `env`'s data dir after `try_lock` took
/// the lock, then sweeps the stale ones. A second lock of this process in
/// the same data dir only counts. Returns the records folder, the key
/// [`release`] takes, or `None` when no record could be written.
pub(crate) fn claim(env: &Env, command: HolderCommand, mode: LockMode) -> Option<PathBuf> {
    let dir = dir(env);
    let mut state = state();
    if let Some(o) = state.records.iter_mut().find(|o| o.dir == dir) {
        o.count += 1;
        return Some(dir);
    }
    let pid = i32::try_from(std::process::id()).ok()?;
    let start = start_us(procs::process_start(pid)?);
    let holder = Holder {
        pid,
        start_us: start,
        started: rfc3339(start),
        command,
        mode,
        origin: origin(),
        phase: matches!(command, HolderCommand::Clean | HolderCommand::CleanItems).then_some(HolderPhase::Preparing),
        progress: None,
    };
    let path = dir.join(record_name(pid, start));
    write(&path, &holder).ok()?;
    state.records.push(Ours {
        dir: dir.clone(),
        path: path.clone(),
        holder,
        count: 1,
        written: Instant::now(),
        dirty: false,
    });
    drop(state);
    sweep_in(&dir, &procs::process_start, procs::uid(), Some(&path));
    Some(dir)
}

/// One lock of this process in `dir` is released: the last one forgets the
/// record. Called from the lock's `Drop`, before the file closes.
pub(crate) fn release(dir: &Path) {
    let mut state = state();
    let Some(i) = state.records.iter().position(|o| o.dir == dir) else { return };
    state.records[i].count -= 1;
    if state.records[i].count == 0 {
        let o = state.records.swap_remove(i);
        forget_file(&o.path);
    }
}

/// Forgets every record of this process, then ends it by `sig`
/// ([`signals::reraise`], which skips `Drop`). The one caller of `reraise`
/// outside the platform modules; call after the report is written.
pub fn end_by_signal(sig: i32) -> ! {
    for o in state().records.drain(..) {
        forget_file(&o.path);
    }
    signals::reraise(sig)
}

/// A clean's executor started: a SIGINT now stops it after its current item.
/// Only a record that has a phase (a clean's) changes.
pub fn note_cleaning() {
    for o in state().records.iter_mut().filter(|o| o.holder.phase.is_some()) {
        o.holder.phase = Some(HolderPhase::Cleaning);
        if write(&o.path, &o.holder).is_ok() {
            o.written = Instant::now();
            o.dirty = false;
        }
    }
}

/// A clean's items done of its total, written as [`should_write`] says. A
/// change held back is written by a flusher thread once the gap passed, so
/// a long item after quick ones shows its own number, not an old one.
pub fn note_progress(done: u64, total: u64) {
    let mut state = state();
    for o in &mut state.records {
        let first = o.holder.progress.is_none();
        o.holder.progress = Some(HolderProgress { done, total });
        o.dirty = true;
        if should_write(first, done >= total, o.written.elapsed()) && write(&o.path, &o.holder).is_ok() {
            o.written = Instant::now();
            o.dirty = false;
        }
    }
    if !state.flushing && state.records.iter().any(|o| o.dirty) {
        state.flushing = std::thread::Builder::new().name("ddc-holders".into()).spawn(flush).is_ok();
    }
}

/// Whether a progress change is written now: the first and the last
/// always, any other once [`PROGRESS_GAP`] passed since the last write.
pub fn should_write(first: bool, last: bool, since_last_write: Duration) -> bool {
    first || last || since_last_write >= PROGRESS_GAP
}

/// The flusher: writes held-back progress once its gap passed, and ends
/// when none is left.
fn flush() {
    loop {
        std::thread::sleep(PROGRESS_GAP);
        let mut state = state();
        for o in state.records.iter_mut().filter(|o| o.dirty && o.written.elapsed() >= PROGRESS_GAP) {
            if write(&o.path, &o.holder).is_ok() {
                o.written = Instant::now();
                o.dirty = false;
            }
        }
        if !state.records.iter().any(|o| o.dirty) {
            state.flushing = false;
            return;
        }
    }
}

fn write(path: &Path, holder: &Holder) -> std::io::Result<PathBuf> {
    let json = serde_json::to_vec_pretty(holder).map_err(std::io::Error::other)?;
    crate::store::write_atomic(path, &json)
}

fn forget_file(path: &Path) {
    #[allow(clippy::disallowed_methods)] // a lock holder's record: ours, or a dead holder's
    let _ = fs::remove_file(path);
}

fn rfc3339(start_us: u64) -> String {
    let at = SystemTime::UNIX_EPOCH + Duration::from_micros(start_us);
    humantime::format_rfc3339_seconds(at).to_string()
}

/// Where this process was started (see [`Origin`]).
fn origin() -> Origin {
    // A terminal first: the cask's `ddc` is a link into the app bundle, and
    // the app never gives the CLI a terminal.
    if std::io::stdin().is_terminal() || std::io::stderr().is_terminal() {
        return Origin::Terminal;
    }
    let in_app = std::env::current_exe()
        .ok()
        .is_some_and(|exe| exe.parent().is_some_and(|d| d.ends_with(Path::new("Contents").join("Helpers"))));
    if in_app { Origin::App } else { Origin::Other }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UID: u32 = 501;

    fn at(us: u64) -> Option<ProcStart> {
        Some(ProcStart { uid: UID, sec: us / 1_000_000, usec: us % 1_000_000 })
    }

    #[test]
    fn a_record_is_live_while_its_pid_runs_as_the_user_with_its_start() {
        let start = 1_792_404_000_000_000;
        assert!(live(start, at(start), UID));
        assert!(live(start, at(start + START_TOLERANCE_US), UID), "boot time moved");
        assert!(live(start, at(start - START_TOLERANCE_US), UID));
        assert!(!live(start, at(start + START_TOLERANCE_US + 1), UID), "the pid was reused");
        assert!(!live(start, None, UID), "no such process");
        assert!(!live(start, Some(ProcStart { uid: UID + 1, sec: start / 1_000_000, usec: 0 }), UID), "another user");
    }

    #[test]
    fn a_record_name_is_its_pid_and_start() {
        assert_eq!(record_name(4101, 1_792_404_000_123_456), "4101-1792404000123456.json");
        assert_eq!(parse_name("4101-1792404000123456.json"), Some((4101, 1_792_404_000_123_456)));
        for other in [".4101-1.json.tmp-7", "4101.json", "a-1.json", "4101-1.txt", "lock"] {
            assert_eq!(parse_name(other), None, "{other}");
        }
    }

    #[test]
    fn progress_is_written_first_last_and_then_once_a_second() {
        let soon = PROGRESS_GAP / 2;
        assert!(should_write(true, false, soon), "the first");
        assert!(should_write(false, true, soon), "the last");
        assert!(!should_write(false, false, soon), "held back");
        assert!(should_write(false, false, PROGRESS_GAP), "the gap passed");
    }

    #[test]
    fn a_temp_file_names_the_record_it_was_for() {
        assert_eq!(temp_of(".4101-1792404000123456.json.tmp-4101"), Some((4101, 1_792_404_000_123_456)));
        for other in ["4101-1.json", ".4101-1.json", ".4101-1.json.tmp-x", ".notes.tmp-1"] {
            assert_eq!(temp_of(other), None, "{other}");
        }
    }

    #[test]
    fn the_start_is_written_in_utc_to_the_second() {
        assert_eq!(rfc3339(1_792_404_000_999_999), "2026-10-19T10:00:00Z");
    }
}
