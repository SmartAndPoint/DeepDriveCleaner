//! Story "Harness data rules" (epic DDC-003), its third step: a coding
//! agent's sessions as the entries of its session folders, and their
//! liveness locks (ADR "Agent footprint: worktrees, harness data and
//! bounded consent for agents", §3). Fake harness folders in a temporary
//! home, never the real ones; Claude Code's records are fakes, and the
//! process table is injected, with fake pids only. The clean of a session
//! entry is in `tests/harness_clean.rs`.

mod common;

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::fakes::{FakeCommands, FakeProcs, FakeTrash};
use common::{TreeBuilder, alloc, id, reference_alloc};
use ddc_core::agents::liveness::{LiveBy, UnknownCause};
use ddc_core::class::RiskClass;
use ddc_core::env::Env;
use ddc_core::exec::preflight::{self, Prediction, PreflightLine, PreflightPorts};
use ddc_core::exec::report::CleanReport;
use ddc_core::exec::{
    self, Consent, ExecOptions, MemoryJournal, OpenVnode, Outcome, Ports, ProcStart, ProcessTable, SkipReason,
};
use ddc_core::model::{Contents, FolderEntry, FolderEntryKind, ScanReport};
use ddc_core::plan::{ActionKind, ListedItem, plan_listed};
use ddc_core::platform::procs::{self, ProcInfo};
use ddc_core::report::contents::{EntryIndex, EntryState, NotAnEntry};
use ddc_core::report::recheck::Reach;
use ddc_core::report::volumes::SystemStatFs;
use ddc_core::report::{self, Processes, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::rules::entries::EntryLock;
use ddc_core::scan::{Progress, StdReader, WalkOptions};

const HOUR: Duration = Duration::from_secs(3_600);

// Fixture session ids, never real ones.
const A: &str = "0a1b2c3d-0000-4000-8000-00000000000a";
const B: &str = "0a1b2c3d-0000-4000-8000-00000000000b";
const C: &str = "0a1b2c3d-0000-4000-8000-00000000000c";
const D: &str = "0a1b2c3d-0000-4000-8000-00000000000d";

/// A project folder of Claude Code's, named after `/Users/example/proj`.
const PROJECT: &str = ".claude/projects/-Users-example-proj";
const HISTORY: &str = ".claude/file-history";
const RECORDS: &str = ".claude/sessions";
const TRANSCRIPTS: &str = "agent.claude-code-transcripts";

struct Home {
    t: TreeBuilder,
    data: tempfile::TempDir,
    /// An hour after the fixtures were written.
    now: SystemTime,
}

impl Home {
    fn new() -> Self {
        Self { t: TreeBuilder::new(), data: tempfile::tempdir().unwrap(), now: SystemTime::now() + HOUR }
    }

    fn env(&self) -> Env {
        Env::with_paths(self.t.root(), self.data.path(), &self.data.path().join("logs"), self.now).unwrap()
    }

    fn catalog(&self) -> Catalog {
        Catalog::builtin(self.t.root())
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.t.path(rel)
    }

    fn scan_with(
        &self,
        procs: impl ProcessTable + Send + Sync + 'static,
        skip: Vec<PathBuf>,
    ) -> (ScanReport, EntryIndex) {
        let opts = ScanOptions {
            walk: WalkOptions { threads: Some(4), ..Default::default() },
            processes: Processes::fixed(procs),
            skip,
            ..Default::default()
        };
        report::scan_with_entries(&self.env(), &opts, &Progress::default(), &SystemStatFs).unwrap()
    }

    fn scan(&self, procs: impl ProcessTable + Send + Sync + 'static) -> (ScanReport, EntryIndex) {
        self.scan_with(procs, Vec::new())
    }

    fn contents<'a>(&self, r: &'a ScanReport, rel: &str) -> &'a Contents {
        let f = r.findings.iter().find(|f| f.path == self.path(rel)).unwrap_or_else(|| panic!("no finding at {rel}"));
        f.contents.as_ref().unwrap_or_else(|| panic!("{rel} lists no contents"))
    }

    /// Each listed entry's name and lock, in order.
    fn locks(&self, r: &ScanReport, rel: &str) -> Vec<(String, Option<EntryLock>)> {
        self.contents(r, rel)
            .entries
            .iter()
            .map(|e| (e.path.file_name().unwrap().to_string_lossy().into_owned(), e.lock.clone()))
            .collect()
    }

    /// Two sessions, `A` and `B`, each with a transcript in the project
    /// folder and a folder in `file-history`; `A` with companions too. The
    /// records folder exists, empty.
    fn two_sessions(&self) {
        self.two_sessions_without_records();
        self.t.dir(RECORDS);
    }

    /// [`Self::two_sessions`] with no records folder: an older Claude Code.
    fn two_sessions_without_records(&self) {
        let t = &self.t;
        t.file(&format!("{PROJECT}/{A}.jsonl"), 20_000);
        t.file(&format!("{PROJECT}/{A}/tool-results/out.txt"), 30_000);
        t.write(&format!("{PROJECT}/{A}.desktop-released.json"), "{}");
        t.file(&format!("{PROJECT}/{B}.jsonl"), 1_000);
        t.file(&format!("{HISTORY}/{A}/0123abcd@v1"), 1_000);
        t.file(&format!("{HISTORY}/{B}/4567cdef@v1"), 1_000);
    }

    /// A Claude Code record for `pid`, started after the process did.
    fn record(&self, pid: i32, session: Option<&str>) {
        let session = session.map(|s| format!(r#", "sessionId": "{s}""#)).unwrap_or_default();
        self.t.write(&format!("{RECORDS}/{pid}.json"), &format!(r#"{{"pid": {pid}, "startedAt": 1000500{session}}}"#));
    }

    /// The session entry as an entry is listed today: the `user-data`
    /// class marks a listed item as an entry of its folder.
    fn listed(&self, rel: &str) -> ListedItem {
        let path = self.path(rel);
        let (dev, ino) = id(&path);
        ListedItem {
            path,
            dev,
            ino,
            rule: TRANSCRIPTS.to_owned(),
            class: RiskClass::UserData,
            action: ActionKind::Trash,
            permanent: false,
        }
    }
}

/// `pid` runs as this user, and started before its record.
fn running(pid: i32) -> FakeProcs {
    FakeProcs::new(vec![ProcInfo { pid, name: "claude".into(), ..ProcInfo::default() }])
        .with_start(pid, ProcStart { uid: procs::uid(), sec: 1000, usec: 0 })
}

fn bytes(path: &Path) -> u64 {
    if std::fs::symlink_metadata(path).unwrap().is_dir() { reference_alloc(path) } else { alloc(path) }
}

/// A table any read of which fails the test.
struct Untouchable;

impl ProcessTable for Untouchable {
    fn processes(&self) -> Vec<ProcInfo> {
        panic!("the process table was read")
    }
    fn open_files(&self, _: i32) -> Vec<PathBuf> {
        panic!("the process table was read")
    }
    fn process_start(&self, _: i32) -> Option<ProcStart> {
        panic!("the process table was read")
    }
    fn open_vnodes(&self, _: i32) -> Vec<OpenVnode> {
        panic!("the process table was read")
    }
}

/// AC 2, the entries: a session's entry is keyed by its transcript (or its
/// folder in `file-history`), holds its companions' bytes and has no floor;
/// a companion without its key, `memory/` and `sessions-index.json` are no
/// entry, summed in the rest; equal sizes go by name.
#[test]
fn sessions_are_entries_keyed_by_the_transcript_with_their_companions() {
    if common::unsupported("sessions_are_entries_keyed_by_the_transcript_with_their_companions") {
        return;
    }
    let h = Home::new();
    let t = &h.t;
    h.two_sessions();
    // `C`'s folder and `D`'s companion, without their transcripts.
    t.file(&format!("{PROJECT}/{C}/subagents/agent-1.jsonl"), 4_000);
    t.write(&format!("{PROJECT}/{D}.desktop-released.json"), "{}");
    t.write(&format!("{PROJECT}/sessions-index.json"), "{}");
    t.write(&format!("{PROJECT}/memory/MEMORY.md"), "- a note\n");
    // `D`'s folder in `file-history` without a key's kind: a file.
    t.file(&format!("{HISTORY}/{D}"), 1_000);

    let (r, index) = h.scan(FakeProcs::new(Vec::new()));

    let p = |name: String| h.path(&format!("{PROJECT}/{name}"));
    let project = h.contents(&r, PROJECT);
    let a = &project.entries[0];
    let a_bytes = [p(format!("{A}.jsonl")), p(A.to_owned()), p(format!("{A}.desktop-released.json"))]
        .iter()
        .map(|f| bytes(f))
        .sum::<u64>();
    let (dev, ino) = id(&p(format!("{A}.jsonl")));
    assert_eq!(
        (&a.path, a.kind, a.bytes, a.dev, a.ino, &a.lock, a.incomplete),
        (&p(format!("{A}.jsonl")), FolderEntryKind::File, a_bytes, dev, ino, &None, false),
        "{a:?}"
    );
    assert!(a.modified.is_some());
    let b: &FolderEntry = &project.entries[1];
    assert_eq!((&b.path, b.bytes), (&p(format!("{B}.jsonl")), alloc(&p(format!("{B}.jsonl")))), "no floor");
    assert_eq!(project.entries.len(), 2, "{:?}", project.entries);
    let rest =
        [p(C.to_owned()), p(format!("{D}.desktop-released.json")), p("sessions-index.json".into()), p("memory".into())];
    assert_eq!((project.rest_count, project.rest_bytes), (4, rest.iter().map(|f| bytes(f)).sum::<u64>()));

    // The index knows each child: the key pickable, the rest never.
    let state = |name: String| {
        let (_, e) = index.entry(&h.path(PROJECT), std::ffi::OsStr::new(&name)).unwrap();
        e.unwrap_or_else(|| panic!("{name} is not indexed")).state.clone()
    };
    assert_eq!(state(format!("{A}.jsonl")), EntryState::Pickable);
    assert_eq!(
        index.entry(&h.path(PROJECT), std::ffi::OsStr::new(&format!("{A}.jsonl"))).unwrap().1.unwrap().bytes,
        a_bytes
    );
    for companion in [A.to_owned(), format!("{A}.desktop-released.json")] {
        assert_eq!(state(companion.clone()), EntryState::NotAnEntry(NotAnEntry::Companion), "{companion}");
    }
    for other in [C.to_owned(), format!("{D}.desktop-released.json"), "sessions-index.json".into(), "memory".into()] {
        assert_eq!(state(other.clone()), EntryState::NotAnEntry(NotAnEntry::NotASession), "{other}");
    }

    // `file-history`: each session's folder, the same size, by name.
    let history = h.contents(&r, HISTORY);
    let names: Vec<_> = history.entries.iter().map(|e| e.path.clone()).collect();
    assert_eq!(names, [h.path(&format!("{HISTORY}/{A}")), h.path(&format!("{HISTORY}/{B}"))]);
    assert!(history.entries.iter().all(|e| e.kind == FolderEntryKind::Folder && e.lock.is_none()));
    assert_eq!(history.entries[0].bytes, history.entries[1].bytes, "a tie, broken by name");
    assert_eq!(history.rest_count, 1, "a file named after a session is no key in file-history");
}

/// AC 4, the scan: a live record locks its session's entries in both
/// folders `live-session`, by record; a session whose file a process holds
/// open is live by open file; a record whose process is gone does not
/// block.
#[test]
fn a_live_record_or_an_open_file_locks_its_session_alone() {
    if common::unsupported("a_live_record_or_an_open_file_locks_its_session_alone") {
        return;
    }
    let h = Home::new();
    h.two_sessions();
    h.t.file(&format!("{PROJECT}/{C}.jsonl"), 1_000);
    h.record(4242, Some(A));
    // 4343 is gone: a crash leftover naming `B`.
    h.record(4343, Some(B));
    let c = id(&h.path(&format!("{PROJECT}/{C}.jsonl")));
    let procs = running(4242);
    let mut table = procs.with_vnodes(4444, vec![OpenVnode { dev: c.0, ino: c.1, path: None }]);
    table.procs.push(ProcInfo { pid: 4444, name: "tail".into(), ..ProcInfo::default() });

    let (r, index) = h.scan(table);

    let live = |s: &str, by| Some(EntryLock::LiveSession { session: s.to_owned(), by });
    let mut project = h.locks(&r, PROJECT);
    project.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        project,
        [
            (format!("{A}.jsonl"), live(A, LiveBy::Record)),
            (format!("{B}.jsonl"), None),
            (format!("{C}.jsonl"), live(C, LiveBy::OpenFile)),
        ]
    );
    assert_eq!(h.locks(&r, HISTORY), [(A.to_owned(), live(A, LiveBy::Record)), (B.to_owned(), None)]);
    let (_, a) = index.entry(&h.path(PROJECT), std::ffi::OsStr::new(&format!("{A}.jsonl"))).unwrap();
    assert_eq!(a.unwrap().state, EntryState::Locked(EntryLock::LiveSession { session: A.into(), by: LiveBy::Record }));
}

/// AC 4, the scan: a session none of whose files changed for ten minutes
/// is free, one whose companion changed a minute ago is live by recent; a
/// session folder never takes the `recent` lock.
#[test]
fn a_session_changed_in_the_last_ten_minutes_is_live_by_recent() {
    if common::unsupported("a_session_changed_in_the_last_ten_minutes_is_live_by_recent") {
        return;
    }
    let h = Home::new();
    h.two_sessions();
    let minute_ago = h.now.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64 - 60;
    h.t.touch(&format!("{PROJECT}/{A}.desktop-released.json"), minute_ago);

    let (r, _) = h.scan(FakeProcs::new(Vec::new()));

    let locks = h.locks(&r, PROJECT);
    assert_eq!(
        locks,
        [
            (format!("{A}.jsonl"), Some(EntryLock::LiveSession { session: A.into(), by: LiveBy::Recent })),
            (format!("{B}.jsonl"), None),
        ]
    );
    assert!(h.locks(&r, HISTORY).iter().all(|(_, l)| l != &Some(EntryLock::Recent)));
}

/// AC 4, the scan, failing closed: a record that does not parse, a live
/// record that names no session, no records folder, or a records folder
/// the user skips, each lock every session `liveness-unknown`.
#[test]
fn records_that_cannot_be_decided_lock_every_session_unknown() {
    if common::unsupported("records_that_cannot_be_decided_lock_every_session_unknown") {
        return;
    }
    let unknown = |cause| Some(EntryLock::LivenessUnknown { cause });
    let every = |h: &Home, r: &ScanReport, cause| {
        for folder in [PROJECT, HISTORY] {
            let locks = h.locks(r, folder);
            assert_eq!(locks.len(), 2, "{folder}");
            assert!(locks.iter().all(|(_, l)| *l == unknown(cause)), "{folder}: {locks:?}");
        }
    };

    let h = Home::new();
    h.two_sessions();
    h.record(4242, Some(A));
    h.t.write(&format!("{RECORDS}/4545.json"), "{\"pid\": 45");
    let (r, _) = h.scan(running(4242));
    every(&h, &r, UnknownCause::UnparsableRecord);

    let h = Home::new();
    h.two_sessions();
    h.record(4242, None);
    let (r, _) = h.scan(running(4242));
    every(&h, &r, UnknownCause::LiveRecordWithoutSession);

    let h = Home::new();
    h.two_sessions_without_records();
    let (r, _) = h.scan(FakeProcs::new(Vec::new()));
    every(&h, &r, UnknownCause::RecordsMissing);

    // A skipped records folder is never opened, even to find it fine.
    let h = Home::new();
    h.two_sessions();
    let (r, _) = h.scan_with(Untouchable, vec![h.path(RECORDS)]);
    every(&h, &r, UnknownCause::RecordsSkipped);
}

/// The liveness check costs a scan nothing without a session: neither the
/// records nor the process table are read.
#[test]
fn liveness_is_not_read_without_a_session() {
    if common::unsupported("liveness_is_not_read_without_a_session") {
        return;
    }
    let h = Home::new();
    h.t.write(&format!("{PROJECT}/sessions-index.json"), "{}");
    h.t.write(&format!("{PROJECT}/memory/MEMORY.md"), "- a note\n");
    h.t.file(&format!("{PROJECT}/{C}/subagents/agent-1.jsonl"), 1_000);
    h.t.write(&format!("{RECORDS}/4545.json"), "{\"pid\": 45");
    h.t.file("Downloads/a.iso", 1_000);

    let (r, _) = h.scan(Untouchable);

    let project = h.contents(&r, PROJECT);
    assert!(project.entries.is_empty());
    assert_eq!(project.rest_count, 3);
}

/// A session's entry listed as its folder's `review` class is no finding
/// of the scan: the plan, the dry run and the clean each refuse it
/// `changed-since-scan`, and it stays.
#[test]
fn the_plan_the_dry_run_and_the_clean_refuse_a_session_entry_listed_as_review() {
    if common::unsupported("the_plan_the_dry_run_and_the_clean_refuse_a_session_entry_listed_as_review") {
        return;
    }
    let h = Home::new();
    h.two_sessions();
    let catalog = h.catalog();
    let (r, index) = h.scan(FakeProcs::new(Vec::new()));
    assert_eq!(h.locks(&r, PROJECT)[1], (format!("{B}.jsonl"), None), "free in the scan");
    let items = [ListedItem { class: RiskClass::Review, ..h.listed(&format!("{PROJECT}/{B}.jsonl")) }];

    let listed = plan_listed(&r, &index, &catalog, &items);
    assert!(listed.plan.items.is_empty());
    assert_eq!(listed.refused.iter().map(|x| x.reason.clone()).collect::<Vec<_>>(), [SkipReason::ChangedSinceScan]);

    let mut predicted = Vec::new();
    let ports =
        PreflightPorts { commands: &FakeCommands::default(), procs: &FakeProcs::new(Vec::new()), reader: &StdReader };
    preflight::preflight(&h.env(), &catalog, &[], &[], &items, &ports, &mut |line| {
        if let PreflightLine::Item(i) = line {
            predicted.push(i.prediction.clone());
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(predicted, [Prediction::WouldSkip(SkipReason::ChangedSinceScan)]);

    let mut journal = MemoryJournal::default();
    let run = exec::execute(
        &h.env(),
        &catalog,
        &listed.plan,
        &Consent::Listed,
        Ports {
            commands: &FakeCommands::default(),
            trash: &FakeTrash(h.data.path().join("FakeTrash")),
            procs: &FakeProcs::new(Vec::new()),
            journal: &mut journal,
        },
        &ExecOptions { cancel: &AtomicBool::new(false), ..ExecOptions::default() },
    )
    .unwrap();
    let report = CleanReport::new(&items, &listed.refused, &run, PathBuf::from("/journal"));
    let outcomes: Vec<_> = report.items.into_iter().map(|i| i.outcome).collect();
    assert_eq!(outcomes, [Outcome::Skipped(SkipReason::ChangedSinceScan)]);
    assert!(h.path(&format!("{PROJECT}/{B}.jsonl")).exists());
}

/// The re-check before a clean walks a session entry's companions, its
/// siblings named after it, and no other session's; a user-data entry's
/// siblings stay unread.
#[test]
fn the_recheck_reaches_a_session_entrys_companions() {
    if common::unsupported("the_recheck_reaches_a_session_entrys_companions") {
        return;
    }
    let h = Home::new();
    let catalog = h.catalog();
    let session = ListedItem {
        path: PathBuf::from(format!("/Users/example/{PROJECT}/{A}.jsonl")),
        dev: 1,
        ino: 2,
        rule: TRANSCRIPTS.to_owned(),
        class: RiskClass::Review,
        action: ActionKind::Trash,
        permanent: false,
    };
    let reach = Reach::new(std::slice::from_ref(&session), &catalog);
    assert!(reach.allows(Path::new(&format!("/Users/example/{PROJECT}/{A}"))));
    assert!(reach.allows(Path::new(&format!("/Users/example/{PROJECT}/{A}/tool-results"))));
    assert!(!reach.allows(Path::new(&format!("/Users/example/{PROJECT}/{B}"))));
    assert!(!reach.allows(Path::new(&format!("/Users/example/{PROJECT}/memory"))));

    let download = ListedItem {
        path: PathBuf::from("/Users/example/Downloads/a"),
        rule: "user.downloads".to_owned(),
        class: RiskClass::UserData,
        ..session
    };
    let reach = Reach::new(&[download], &catalog);
    assert!(reach.allows(Path::new("/Users/example/Downloads/a/x")));
    assert!(!reach.allows(Path::new("/Users/example/Downloads/b")));
}
