//! Story "Harness data rules" (epic DDC-003), its fourth step: a coding
//! agent's session entry goes to the Trash, its companions first and its
//! transcript last, and a harness that is live keeps its folders (ADR
//! "Agent footprint: worktrees, harness data and bounded consent for
//! agents", §3). Fake harness folders in a temporary home, never the real
//! ones; Claude Code's records and Codex's locks are fakes, the process
//! table is injected with fake pids only, and the Trash is a folder of the
//! test.

mod common;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime};

use common::TreeBuilder;
use common::fakes::{FakeCommands, FakeProcs};
use ddc_core::agents::HarnessId;
use ddc_core::agents::liveness::{LiveBy, UnknownCause};
use ddc_core::class::RiskClass;
use ddc_core::env::Env;
use ddc_core::exec::preflight::{self, Prediction, PreflightLine, PreflightPorts};
use ddc_core::exec::{
    self, AgentLiveBy, Consent, ExecOptions, ExecReport, JournalEntry, MemoryJournal, OpenVnode, Outcome, Ports,
    ProcStart, ProcessTable, SkipReason, Trasher,
};
use ddc_core::model::ScanReport;
use ddc_core::plan::{Action, ActionKind, ListedItem, ListedPlan, plan_listed_with};
use ddc_core::platform::procs::{self, ProcInfo};
use ddc_core::report::contents::EntryIndex;
use ddc_core::report::volumes::SystemStatFs;
use ddc_core::report::{self, Processes, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::rules::entries::EntryLock;
use ddc_core::scan::{Progress, StdReader, WalkOptions};

const HOUR: Duration = Duration::from_secs(3_600);

// Fixture session ids, never real ones.
const A: &str = "0a1b2c3d-0000-4000-8000-00000000000a";
const B: &str = "0a1b2c3d-0000-4000-8000-00000000000b";

/// A project folder of Claude Code's, named after `/Users/example/proj`.
const PROJECT: &str = ".claude/projects/-Users-example-proj";
const HISTORY: &str = ".claude/file-history";
const RECORDS: &str = ".claude/sessions";
const TRANSCRIPTS: &str = "agent.claude-code-transcripts";
const LOCKS: &str = ".codex/thread-writer-locks";

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

    fn scan(&self) -> (ScanReport, EntryIndex) {
        self.scan_with(FakeProcs::new(Vec::new()), Vec::new())
    }

    /// Two sessions, `A` and `B`, each with a transcript in the project
    /// folder and a folder in `file-history`; `A`'s transcript with two
    /// companions, its folder of tool results and a sidecar file. The
    /// records folder exists, empty.
    fn two_sessions(&self) {
        let t = &self.t;
        t.file(&format!("{PROJECT}/{A}.jsonl"), 20_000);
        t.file(&format!("{PROJECT}/{A}/tool-results/out.txt"), 30_000);
        t.write(&format!("{PROJECT}/{A}.desktop-released.json"), "{}");
        t.file(&format!("{PROJECT}/{B}.jsonl"), 1_000);
        t.file(&format!("{HISTORY}/{A}/0123abcd@v1"), 1_000);
        t.file(&format!("{HISTORY}/{B}/4567cdef@v1"), 1_000);
        t.dir(RECORDS);
    }

    /// A Claude Code record for `pid`, started after the process did.
    fn record(&self, pid: i32, session: &str) {
        self.t.write(
            &format!("{RECORDS}/{pid}.json"),
            &format!(r#"{{"pid": {pid}, "startedAt": 1000500, "sessionId": "{session}"}}"#),
        );
    }

    /// A session's entry as the app lists it: the `user-data` class marks
    /// an entry of its folder.
    fn entry(&self, rel: &str, rule: &str) -> ListedItem {
        self.item(rel, rule, RiskClass::UserData)
    }

    /// A whole folder of a `review` rule, to the Trash.
    fn folder(&self, rel: &str, rule: &str) -> ListedItem {
        self.item(rel, rule, RiskClass::Review)
    }

    fn item(&self, rel: &str, rule: &str, class: RiskClass) -> ListedItem {
        let path = self.path(rel);
        let (dev, ino) = common::id(&path);
        ListedItem { path, dev, ino, rule: rule.to_owned(), class, action: ActionKind::Trash, permanent: false }
    }

    fn plan(&self, items: &[ListedItem], procs: &dyn ProcessTable) -> ListedPlan {
        let (r, index) = self.scan();
        plan_listed_with(&r, &index, &self.catalog(), items, procs, &[])
    }

    fn run(
        &self,
        plan: &ListedPlan,
        trash: &dyn Trasher,
        procs: &dyn ProcessTable,
        skips: &[PathBuf],
    ) -> (ExecReport, Vec<JournalEntry>) {
        let mut journal = MemoryJournal::default();
        let run = exec::execute(
            &self.env(),
            &self.catalog(),
            &plan.plan,
            &Consent::Listed,
            Ports { commands: &FakeCommands::default(), trash, procs, journal: &mut journal },
            &ExecOptions { cancel: &AtomicBool::new(false), skips, ..ExecOptions::default() },
        )
        .unwrap();
        (run, journal.0)
    }

    /// The dry run's prediction of each item, in list order.
    fn predict(
        &self,
        items: &[ListedItem],
        procs: &dyn ProcessTable,
        skips: &[PathBuf],
    ) -> Vec<(Option<u64>, Prediction)> {
        let mut predicted = Vec::new();
        let ports = PreflightPorts { commands: &FakeCommands::default(), procs, reader: &StdReader };
        preflight::preflight_skipping(&self.env(), &self.catalog(), &[], &[], skips, items, &ports, &mut |line| {
            if let PreflightLine::Item(i) = line {
                predicted.push((i.bytes, i.prediction.clone()));
            }
            Ok(())
        })
        .unwrap();
        predicted
    }

    /// A Trash of the test: each path moved into it is recorded in order,
    /// and one named `fail` is refused.
    fn trash(&self, fail: Option<&str>) -> RecordingTrash {
        RecordingTrash { dir: self.data.path().join("Trash"), fail: fail.map(OsString::from), moved: Mutex::default() }
    }
}

struct RecordingTrash {
    dir: PathBuf,
    fail: Option<OsString>,
    moved: Mutex<Vec<PathBuf>>,
}

impl Trasher for RecordingTrash {
    fn trash(&self, path: &Path) -> std::io::Result<()> {
        if path.file_name() == self.fail.as_deref() {
            return Err(std::io::Error::other("the Trash refused it"));
        }
        // Numbered, as two sessions' folders share a name.
        let mut moved = self.moved.lock().unwrap();
        let to = self.dir.join(moved.len().to_string());
        std::fs::create_dir_all(&to)?;
        std::fs::rename(path, to.join(path.file_name().unwrap()))?;
        moved.push(path.to_path_buf());
        Ok(())
    }
}

impl RecordingTrash {
    fn moved(&self) -> Vec<PathBuf> {
        self.moved.lock().unwrap().clone()
    }
}

/// `pid` runs as this user, and started before its record.
fn claude(pid: i32) -> FakeProcs {
    FakeProcs::new(vec![ProcInfo { pid, name: "claude".into(), ..ProcInfo::default() }])
        .with_start(pid, ProcStart { uid: procs::uid(), sec: 1000, usec: 0 })
}

fn process(pid: i32, name: &str, exe: Option<&str>, args: &[&str]) -> ProcInfo {
    ProcInfo {
        pid,
        name: name.to_owned(),
        exe: exe.map(PathBuf::from),
        args: args.iter().map(OsString::from).collect(),
        ..ProcInfo::default()
    }
}

fn outcomes(run: &ExecReport) -> Vec<Outcome> {
    run.items.iter().map(|i| i.outcome.clone()).collect()
}

fn reasons(plan: &ListedPlan) -> Vec<SkipReason> {
    plan.refused.iter().map(|r| r.reason.clone()).collect()
}

fn entry_of(line: &JournalEntry) -> Option<String> {
    let value = serde_json::to_value(line).unwrap();
    value.get("entry").map(|e| e.as_str().unwrap().to_owned())
}

/// AC 3: a session's entry goes to the Trash, its companions first, by
/// name, its transcript last; one journal line per moved path, `entry` on
/// the companions' only, and the bytes of the lines add up to the item's.
/// A session's folder in `file-history` goes as one entry; the other
/// session stays.
#[test]
fn a_session_entry_goes_to_the_trash_companions_first_transcript_last() {
    if common::unsupported("a_session_entry_goes_to_the_trash_companions_first_transcript_last") {
        return;
    }
    let h = Home::new();
    h.two_sessions();
    let transcript = format!("{PROJECT}/{A}.jsonl");
    let items =
        [h.entry(&transcript, TRANSCRIPTS), h.entry(&format!("{HISTORY}/{A}"), "agent.claude-code-file-history")];
    let none = FakeProcs::new(Vec::new());
    let plan = h.plan(&items, &none);
    assert!(plan.refused.is_empty(), "{:?}", plan.refused);
    assert!(plan.plan.items.iter().all(|i| i.action == Action::TrashEntry));
    let predicted = h.predict(&items, &none, &[]);

    let trash = h.trash(None);
    let (run, journal) = h.run(&plan, &trash, &none, &[]);

    assert_eq!(outcomes(&run), [Outcome::Trashed, Outcome::Trashed]);
    assert_eq!(
        trash.moved(),
        [
            h.path(&format!("{PROJECT}/{A}")),
            h.path(&format!("{PROJECT}/{A}.desktop-released.json")),
            h.path(&transcript),
            h.path(&format!("{HISTORY}/{A}")),
        ]
    );
    let lines: Vec<(PathBuf, Option<String>, Outcome)> =
        journal.iter().map(|l| (l.path.clone(), entry_of(l), l.outcome.clone())).collect();
    let key = h.path(&transcript).to_string_lossy().into_owned();
    assert_eq!(
        lines,
        [
            (h.path(&format!("{PROJECT}/{A}")), Some(key.clone()), Outcome::Trashed),
            (h.path(&format!("{PROJECT}/{A}.desktop-released.json")), Some(key), Outcome::Trashed),
            (h.path(&transcript), None, Outcome::Trashed),
            (h.path(&format!("{HISTORY}/{A}")), None, Outcome::Trashed),
        ]
    );
    assert!(journal.iter().all(|l| l.rule == run.items[0].rule || l.path == h.path(&format!("{HISTORY}/{A}"))));
    let transcript_lines: u64 = journal[..3].iter().map(|l| l.bytes).sum();
    assert_eq!(transcript_lines, run.items[0].bytes, "the lines add up to the entry");
    assert_eq!(journal[3].bytes, run.items[1].bytes);
    // The dry run predicted it, with the clean's bytes.
    assert_eq!(
        predicted,
        [(Some(run.items[0].bytes), Prediction::WouldTrash), (Some(run.items[1].bytes), Prediction::WouldTrash)]
    );
    for stays in [format!("{PROJECT}/{B}.jsonl"), format!("{HISTORY}/{B}")] {
        assert!(h.path(&stays).exists(), "{stays}");
    }
}

/// AC 3: the first failure stops the entry: the companion moved before it
/// is journaled and stays in the Trash, the transcript stays where it is.
#[test]
fn a_companion_that_fails_stops_the_entry_and_the_transcript_stays() {
    if common::unsupported("a_companion_that_fails_stops_the_entry_and_the_transcript_stays") {
        return;
    }
    let h = Home::new();
    h.two_sessions();
    let transcript = format!("{PROJECT}/{A}.jsonl");
    let items = [h.entry(&transcript, TRANSCRIPTS)];
    let none = FakeProcs::new(Vec::new());
    let plan = h.plan(&items, &none);
    let trash = h.trash(Some(&format!("{A}.desktop-released.json")));

    let (run, journal) = h.run(&plan, &trash, &none, &[]);

    assert!(matches!(&outcomes(&run)[..], [Outcome::Failed { .. }]), "{:?}", outcomes(&run));
    assert_eq!(trash.moved(), [h.path(&format!("{PROJECT}/{A}"))]);
    assert!(h.path(&transcript).exists() && h.path(&format!("{PROJECT}/{A}.desktop-released.json")).exists());
    let lines: Vec<(PathBuf, Option<String>, bool)> =
        journal.iter().map(|l| (l.path.clone(), entry_of(l), matches!(l.outcome, Outcome::Failed { .. }))).collect();
    assert_eq!(
        lines,
        [
            (h.path(&format!("{PROJECT}/{A}")), Some(h.path(&transcript).to_string_lossy().into_owned()), false),
            (h.path(&transcript), None, true),
        ]
    );
}

/// AC 3: a session's entry is never deleted: `--permanent` or `delete` is
/// `action-not-allowed` in the plan and the dry run, and a forged plan's
/// delete, or its move outside a person's list, is refused by the clean.
#[test]
fn a_session_entry_is_never_deleted() {
    if common::unsupported("a_session_entry_is_never_deleted") {
        return;
    }
    let h = Home::new();
    h.two_sessions();
    let transcript = format!("{PROJECT}/{A}.jsonl");
    let delete = ListedItem { action: ActionKind::Delete, ..h.entry(&transcript, TRANSCRIPTS) };
    let permanent = ListedItem { permanent: true, ..delete.clone() };
    let items = [delete, permanent];
    let none = FakeProcs::new(Vec::new());

    let plan = h.plan(&items, &none);
    assert!(plan.plan.items.is_empty());
    assert_eq!(reasons(&plan), [SkipReason::ActionNotAllowed, SkipReason::ActionNotAllowed]);
    let predicted: Vec<Prediction> = h.predict(&items, &none, &[]).into_iter().map(|(_, p)| p).collect();
    assert_eq!(predicted, vec![Prediction::WouldSkip(SkipReason::ActionNotAllowed); 2]);

    let mut forged = h.plan(&[h.entry(&transcript, TRANSCRIPTS)], &none);
    let mut item = forged.plan.items[0].clone();
    item.action = Action::Delete { make_writable: false };
    forged.plan.items.push(item);
    forged.plan.items.remove(0);
    let (run, _) = h.run(&forged, &h.trash(None), &none, &[]);
    assert_eq!(outcomes(&run), [Outcome::Skipped(SkipReason::ActionNotAllowed)]);

    let plan = h.plan(&[h.entry(&transcript, TRANSCRIPTS)], &none);
    let mut journal = MemoryJournal::default();
    let run = exec::execute(
        &h.env(),
        &h.catalog(),
        &plan.plan,
        &Consent::Interactive,
        Ports { commands: &FakeCommands::default(), trash: &h.trash(None), procs: &none, journal: &mut journal },
        &ExecOptions::default(),
    )
    .unwrap();
    assert_eq!(outcomes(&run), [Outcome::Skipped(SkipReason::ActionNotAllowed)]);
    assert!(h.path(&transcript).exists() && h.path(&format!("{PROJECT}/{A}")).exists());
}

/// AC 4: the guard reads the liveness again: a record naming the session
/// written after the plan leaves the entry `live-session`, nothing moved.
#[test]
fn a_record_written_after_the_plan_keeps_the_session() {
    if common::unsupported("a_record_written_after_the_plan_keeps_the_session") {
        return;
    }
    let h = Home::new();
    h.two_sessions();
    let items = [h.entry(&format!("{PROJECT}/{A}.jsonl"), TRANSCRIPTS)];
    let plan = h.plan(&items, &FakeProcs::new(Vec::new()));
    assert_eq!(plan.plan.items.len(), 1, "{:?}", plan.refused);

    h.record(4242, A);
    let trash = h.trash(None);
    let (run, journal) = h.run(&plan, &trash, &claude(4242), &[]);

    let live = SkipReason::EntryLocked { lock: EntryLock::LiveSession { session: A.to_owned(), by: LiveBy::Record } };
    assert_eq!(outcomes(&run), [Outcome::Skipped(live.clone())]);
    assert!(trash.moved().is_empty());
    assert_eq!(journal.len(), 1);
    assert_eq!(h.predict(&items, &claude(4242), &[]), [(None, Prediction::WouldSkip(live))]);
}

/// AC 4: liveness that cannot be told keeps the session, at the plan (the
/// scan's lock), at the guard (a record that cannot be parsed, written
/// after the plan) and under a skipped records folder, which is never read;
/// the dry run says the same.
#[test]
fn a_session_whose_liveness_is_unknown_stays() {
    if common::unsupported("a_session_whose_liveness_is_unknown_stays") {
        return;
    }
    let none = FakeProcs::new(Vec::new());
    let unknown = |cause| SkipReason::EntryLocked { lock: EntryLock::LivenessUnknown { cause } };

    // At the plan: the scan's lock.
    let h = Home::new();
    h.two_sessions();
    h.t.write(&format!("{RECORDS}/9.json"), "not a record");
    let items = [h.entry(&format!("{PROJECT}/{A}.jsonl"), TRANSCRIPTS)];
    let plan = h.plan(&items, &none);
    assert_eq!(reasons(&plan), [unknown(UnknownCause::UnparsableRecord)]);
    assert_eq!(h.predict(&items, &none, &[]), [(None, Prediction::WouldSkip(unknown(UnknownCause::UnparsableRecord)))]);

    // At the guard: written after the plan.
    let h = Home::new();
    h.two_sessions();
    let items = [h.entry(&format!("{PROJECT}/{A}.jsonl"), TRANSCRIPTS)];
    let plan = h.plan(&items, &none);
    assert_eq!(plan.plan.items.len(), 1, "{:?}", plan.refused);
    h.t.write(&format!("{RECORDS}/9.json"), "not a record");
    let (run, _) = h.run(&plan, &h.trash(None), &none, &[]);
    assert_eq!(outcomes(&run), [Outcome::Skipped(unknown(UnknownCause::UnparsableRecord))]);

    // Skipped: the records are not read, by the guard or the dry run.
    let h = Home::new();
    h.two_sessions();
    let items = [h.entry(&format!("{PROJECT}/{A}.jsonl"), TRANSCRIPTS)];
    let plan = h.plan(&items, &none);
    let skips = [h.path(RECORDS)];
    assert_eq!(
        h.predict(&items, &none, &skips),
        [(None, Prediction::WouldSkip(unknown(UnknownCause::RecordsSkipped)))]
    );
    let (run, _) = h.run(&plan, &h.trash(None), &none, &skips);
    assert_eq!(outcomes(&run), [Outcome::Skipped(unknown(UnknownCause::RecordsSkipped))]);
    assert!(h.path(&format!("{PROJECT}/{A}.jsonl")).exists());
}

/// AC 4: `paste-cache` is held by no session: it moves while a live record
/// keeps the session it names, in the same clean.
#[test]
fn paste_cache_moves_while_a_session_is_live() {
    if common::unsupported("paste_cache_moves_while_a_session_is_live") {
        return;
    }
    let h = Home::new();
    h.two_sessions();
    h.t.file(".claude/paste-cache/abc.txt", 1_000);
    let items = [
        h.entry(&format!("{PROJECT}/{A}.jsonl"), TRANSCRIPTS),
        h.folder(".claude/paste-cache", "agent.claude-code-history"),
    ];
    let plan = h.plan(&items, &FakeProcs::new(Vec::new()));
    h.record(4242, A);

    let (run, _) = h.run(&plan, &h.trash(None), &claude(4242), &[]);

    let live = SkipReason::EntryLocked { lock: EntryLock::LiveSession { session: A.to_owned(), by: LiveBy::Record } };
    assert_eq!(outcomes(&run), [Outcome::Skipped(live), Outcome::Trashed]);
    assert!(!h.path(".claude/paste-cache").exists());
}

/// AC 2 and 4: a `.ddc-keep` deep inside a session's folder keeps the
/// session: the scan's lock, and the guard's when it appears after the plan.
#[test]
fn a_keep_marker_deep_in_a_session_folder_keeps_the_session() {
    if common::unsupported("a_keep_marker_deep_in_a_session_folder_keeps_the_session") {
        return;
    }
    let none = FakeProcs::new(Vec::new());
    let kept = SkipReason::EntryLocked { lock: EntryLock::Kept };
    let marker = format!("{PROJECT}/{A}/tool-results/deep/.ddc-keep");

    let h = Home::new();
    h.two_sessions();
    h.t.write(&marker, "");
    let items = [h.entry(&format!("{PROJECT}/{A}.jsonl"), TRANSCRIPTS)];
    let (r, _) = h.scan();
    let folder = r.findings.iter().find(|f| f.path == h.path(PROJECT)).unwrap();
    let a = folder.contents.as_ref().unwrap().entries.iter().find(|e| e.path == items[0].path).unwrap();
    assert_eq!(a.lock, Some(EntryLock::Kept));
    assert_eq!(reasons(&h.plan(&items, &none)), std::slice::from_ref(&kept));
    assert_eq!(h.predict(&items, &none, &[]), [(None, Prediction::WouldSkip(kept.clone()))]);

    let h = Home::new();
    h.two_sessions();
    let items = [h.entry(&format!("{PROJECT}/{A}.jsonl"), TRANSCRIPTS)];
    let plan = h.plan(&items, &none);
    h.t.write(&marker, "");
    let trash = h.trash(None);
    let (run, _) = h.run(&plan, &trash, &none, &[]);
    assert_eq!(outcomes(&run), [Outcome::Skipped(kept)]);
    assert!(trash.moved().is_empty());
}

/// AC 5: Codex, opencode and Cursor's folders go to the Trash while their
/// harness is not live, and are `agent-live` at the plan, the dry run and
/// the guard while it is: a process of it running, a Codex thread lock
/// held open, or a binary of it ddc cannot name.
#[test]
fn a_live_harness_keeps_its_folders() {
    if common::unsupported("a_live_harness_keeps_its_folders") {
        return;
    }
    let folders: [(&str, &str, HarnessId); 4] = [
        (".codex/sessions", "agent.codex-sessions", HarnessId::Codex),
        (".cache/codex-runtimes", "agent.codex-runtimes", HarnessId::Codex),
        (".local/share/opencode/storage", "agent.opencode-data", HarnessId::Opencode),
        (".cursor/projects", "agent.cursor-agent-data", HarnessId::Cursor),
    ];
    let fixture = || {
        let h = Home::new();
        for (rel, ..) in folders {
            h.t.file(&format!("{rel}/payload"), 1_000);
        }
        let lock = h.t.file(&format!("{LOCKS}/thread-1.lock"), 0);
        (h, lock)
    };
    let items = |h: &Home| -> Vec<ListedItem> { folders.iter().map(|(rel, rule, _)| h.folder(rel, rule)).collect() };
    let live = |harness, by| SkipReason::AgentLive { harness, by };

    // Each harness, and what shows it live.
    let (h, lock) = fixture();
    let (dev, ino) = common::id(&lock);
    let holder = FakeProcs::new(vec![process(31, "node", None, &[])])
        .with_vnodes(31, vec![OpenVnode { dev, ino, path: Some(lock.clone()) }]);
    let cases: Vec<(&str, FakeProcs, [Option<SkipReason>; 4])> = vec![
        (
            "a lock held",
            holder,
            [
                Some(live(HarnessId::Codex, AgentLiveBy::LockHeld)),
                Some(live(HarnessId::Codex, AgentLiveBy::LockHeld)),
                None,
                None,
            ],
        ),
        (
            "the app-server",
            FakeProcs::new(vec![process(32, "codex", None, &["codex", "app-server"])]),
            [
                Some(live(HarnessId::Codex, AgentLiveBy::Running)),
                Some(live(HarnessId::Codex, AgentLiveBy::Running)),
                None,
                None,
            ],
        ),
        (
            "a codex in an app",
            FakeProcs::new(vec![process(33, "codex", Some("/Applications/Example.app/Contents/Resources/codex"), &[])]),
            [
                Some(live(HarnessId::Codex, AgentLiveBy::Unknown)),
                Some(live(HarnessId::Codex, AgentLiveBy::Unknown)),
                None,
                None,
            ],
        ),
        (
            "opencode",
            FakeProcs::new(vec![process(34, "opencode", None, &[])]),
            [None, None, Some(live(HarnessId::Opencode, AgentLiveBy::Running)), None],
        ),
        (
            "cursor-agent",
            FakeProcs::new(vec![process(35, "cursor-agent", None, &[])]),
            [None, None, None, Some(live(HarnessId::Cursor, AgentLiveBy::Running))],
        ),
    ];
    for (what, procs, expected) in &cases {
        let listed = items(&h);
        let plan = h.plan(&listed, procs);
        let refused: Vec<Option<SkipReason>> = listed
            .iter()
            .map(|i| plan.refused.iter().find(|r| r.item.path == i.path).map(|r| r.reason.clone()))
            .collect();
        assert_eq!(&refused[..], &expected[..], "{what}: the plan");
        let predicted: Vec<Option<SkipReason>> = h
            .predict(&listed, procs, &[])
            .into_iter()
            .map(|(_, p)| match p {
                Prediction::WouldSkip(reason) => Some(reason),
                _ => None,
            })
            .collect();
        assert_eq!(&predicted[..], &expected[..], "{what}: the dry run");
    }

    // At the guard: started after the plan, nothing moves.
    let (h, _) = fixture();
    let listed = items(&h);
    let plan = h.plan(&listed, &FakeProcs::new(Vec::new()));
    assert_eq!(plan.plan.items.len(), 4, "{:?}", plan.refused);
    let all = FakeProcs::new(vec![
        process(32, "codex", None, &["codex", "app-server"]),
        process(34, "opencode", None, &[]),
        process(35, "cursor-agent", None, &[]),
    ]);
    let trash = h.trash(None);
    let (run, _) = h.run(&plan, &trash, &all, &[]);
    assert_eq!(
        outcomes(&run),
        [
            Outcome::Skipped(live(HarnessId::Codex, AgentLiveBy::Running)),
            Outcome::Skipped(live(HarnessId::Codex, AgentLiveBy::Running)),
            Outcome::Skipped(live(HarnessId::Opencode, AgentLiveBy::Running)),
            Outcome::Skipped(live(HarnessId::Cursor, AgentLiveBy::Running)),
        ]
    );
    assert!(trash.moved().is_empty());

    // None live: each folder goes, one line each and no `entry`.
    let (run, journal) = h.run(&plan, &trash, &FakeProcs::new(Vec::new()), &[]);
    assert_eq!(outcomes(&run), vec![Outcome::Trashed; 4]);
    assert_eq!(trash.moved(), listed.iter().map(|i| i.path.clone()).collect::<Vec<_>>());
    assert!(journal.iter().all(|l| entry_of(l).is_none()), "{journal:?}");
}
