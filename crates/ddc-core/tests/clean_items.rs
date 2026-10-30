//! `clean --items` at the core: the listed plan, cancellation and the dry run
//! (story "CLI: clean an explicit item list"). Real filesystem in temp dirs;
//! commands, the Trash and processes are fakes.

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use common::TreeBuilder;
use common::fakes::{FakeCommands, FakeProcs, FakeTrash, app_process};
use ddc_core::class::RiskClass;
use ddc_core::env::Env;
use ddc_core::exec::preflight::{self, Prediction, PreflightLine, PreflightPorts};
use ddc_core::exec::report::CleanReport;
use ddc_core::exec::{
    self, Consent, ExecOptions, ExecReport, Journal, JournalEntry, MemoryJournal, Outcome, Ports, ProcessTable,
    SkipReason,
};
use ddc_core::model::{RemovalKind, ScanReport};
use ddc_core::plan::{ActionKind, ListError, ListedItem, ListedItems, plan_listed};
use ddc_core::report::{self, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::scan::{DirListing, DirReader, Progress, ReadError, StdReader, Times, WalkOptions};

struct Fx {
    t: TreeBuilder,
    data: tempfile::TempDir,
}

impl Fx {
    fn new() -> Self {
        Self { t: TreeBuilder::new(), data: tempfile::tempdir().unwrap() }
    }

    fn env(&self) -> Env {
        Env::with_paths(self.t.root(), self.data.path(), &self.data.path().join("logs"), SystemTime::now()).unwrap()
    }

    fn catalog(&self) -> Catalog {
        Catalog::builtin(self.t.root())
    }

    fn scan(&self) -> ScanReport {
        let opts = ScanOptions { walk: WalkOptions { threads: Some(2), ..Default::default() }, ..Default::default() };
        report::scan(&self.env(), &opts, &Progress::default()).unwrap()
    }

    /// `node_modules` of a project without a lock file: `review`, Trash by default.
    fn review_modules(&self, rel: &str) -> PathBuf {
        self.t.write(&format!("{rel}/package.json"), "{}");
        self.t.file(&format!("{rel}/node_modules/dep/index.js"), 4000);
        self.t.path(&format!("{rel}/node_modules"))
    }

    fn pycache(&self, rel: &str) -> PathBuf {
        self.t.file(&format!("{rel}/__pycache__/m.pyc"), 2000);
        self.t.path(&format!("{rel}/__pycache__"))
    }

    fn trash(&self) -> FakeTrash {
        FakeTrash(self.data.path().join("FakeTrash"))
    }

    /// Rescans, plans the list and executes it as `clean --items` does.
    fn clean(
        &self,
        items: &[ListedItem],
        commands: &FakeCommands,
        journal: &mut dyn Journal,
        cancel: &AtomicBool,
    ) -> (ExecReport, CleanReport) {
        let catalog = self.catalog();
        let opts = ScanOptions { walk: WalkOptions { threads: Some(2), ..Default::default() }, ..Default::default() };
        let (scanned, entries) =
            report::scan_with_entries(&self.env(), &opts, &Progress::default(), &report::volumes::SystemStatFs)
                .unwrap();
        let listed = plan_listed(&scanned, &entries, &catalog, items);
        let trash = self.trash();
        let run = exec::execute(
            &self.env(),
            &catalog,
            &listed.plan,
            &Consent::Listed,
            Ports { commands, trash: &trash, procs: &FakeProcs::new(vec![]), journal },
            &ExecOptions { cancel, ..ExecOptions::default() },
        )
        .unwrap();
        let clean = CleanReport::new(items, &listed.refused, &run, PathBuf::from("/journal"));
        (run, clean)
    }

    fn preflight(&self, items: &[ListedItem], procs: &dyn ProcessTable, commands: &FakeCommands) -> Vec<PreflightLine> {
        let mut lines = Vec::new();
        let ports = PreflightPorts { commands, procs, reader: &StdReader };
        preflight::preflight(&self.env(), &self.catalog(), &[], &[], items, &ports, &mut |l| {
            lines.push(l.clone());
            Ok(())
        })
        .unwrap();
        lines
    }

    /// path → (is a directory, inode) for everything in the tree.
    fn snapshot(&self) -> BTreeMap<PathBuf, (bool, u64)> {
        let mut out = BTreeMap::new();
        let mut stack = vec![self.t.root().to_path_buf()];
        while let Some(dir) = stack.pop() {
            for e in fs::read_dir(&dir).unwrap() {
                let md = fs::symlink_metadata(e.as_ref().unwrap().path()).unwrap();
                let path = e.unwrap().path();
                out.insert(path.clone(), (md.is_dir(), common::id(&path).1));
                if md.is_dir() {
                    stack.push(path);
                }
            }
        }
        out
    }
}

/// The finding at `path` as the app lists it, with its action as the report names it.
fn listed(r: &ScanReport, path: &Path) -> ListedItem {
    let f = r.findings.iter().find(|f| f.path == path).unwrap_or_else(|| panic!("no finding at {path:?}"));
    let action = match f.removal {
        RemovalKind::Delete | RemovalKind::None => ActionKind::Delete,
        RemovalKind::Trash => ActionKind::Trash,
        RemovalKind::Native => ActionKind::Native,
        RemovalKind::NativeOnly => ActionKind::NativeOnly,
    };
    ListedItem {
        path: f.path.clone(),
        dev: f.dev,
        ino: f.ino,
        rule: f.rule.clone(),
        class: f.class,
        action,
        permanent: false,
    }
}

fn permanently(mut item: ListedItem) -> ListedItem {
    item.action = ActionKind::Delete;
    item.permanent = true;
    item
}

fn outcome<'a>(r: &'a CleanReport, path: &Path) -> &'a Outcome {
    &r.items.iter().find(|i| i.path == path).unwrap_or_else(|| panic!("{path:?} not reported")).outcome
}

#[test]
fn a_review_item_goes_to_the_trash_unless_permanent_and_a_reclassified_one_is_skipped() {
    if common::unsupported("a_review_item_goes_to_the_trash_unless_permanent_and_a_reclassified_one_is_skipped") {
        return;
    }
    let fx = Fx::new();
    let to_trash = fx.review_modules("a");
    let to_delete = fx.review_modules("b");
    let gains_lock = fx.review_modules("c");
    let reviewed = fx.scan();
    let items =
        vec![listed(&reviewed, &to_trash), permanently(listed(&reviewed, &to_delete)), listed(&reviewed, &gains_lock)];
    assert!(items.iter().all(|i| i.class == RiskClass::Review), "{items:#?}");
    assert_eq!(items[0].action, ActionKind::Trash);
    // A lock file appears after the review: the fresh scan says rebuildable.
    fx.t.write("c/package-lock.json", "{}");

    let mut journal = MemoryJournal::default();
    let (_, r) = fx.clean(&items, &FakeCommands::default(), &mut journal, &AtomicBool::new(false));

    assert_eq!(outcome(&r, &to_trash), &Outcome::Trashed);
    assert!(fx.trash().0.join("node_modules").exists() && !to_trash.exists());
    assert_eq!(outcome(&r, &to_delete), &Outcome::Deleted);
    assert!(!to_delete.exists());
    assert_eq!(outcome(&r, &gains_lock), &Outcome::Skipped(SkipReason::ChangedSinceScan));
    assert!(gains_lock.exists(), "not deleted although a rebuildable item would be");
    assert_eq!(journal.0.len(), 2, "the plan-level refusal touches nothing and is not journaled");
}

/// Sets the cancel flag when the first item is journaled.
struct CancelOnFirst<'a> {
    flag: &'a AtomicBool,
    inner: MemoryJournal,
}

impl Journal for CancelOnFirst<'_> {
    fn record(&mut self, entry: &JournalEntry) -> std::io::Result<()> {
        self.flag.store(true, Ordering::SeqCst);
        self.inner.record(entry)
    }
}

#[test]
fn a_cancel_after_the_first_of_three_items_leaves_the_others_not_run() {
    if common::unsupported("a_cancel_after_the_first_of_three_items_leaves_the_others_not_run") {
        return;
    }
    let fx = Fx::new();
    let caches = [fx.pycache("q1"), fx.pycache("q2"), fx.pycache("q3")];
    let reviewed = fx.scan();
    let items: Vec<ListedItem> = caches.iter().map(|c| listed(&reviewed, c)).collect();
    let flag = AtomicBool::new(false);
    let mut journal = CancelOnFirst { flag: &flag, inner: MemoryJournal::default() };

    let (run, r) = fx.clean(&items, &FakeCommands::default(), &mut journal, &flag);

    assert_eq!(outcome(&r, &caches[0]), &Outcome::Deleted);
    assert_eq!(outcome(&r, &caches[1]), &Outcome::NotRun);
    assert_eq!(outcome(&r, &caches[2]), &Outcome::NotRun);
    assert!(caches[1].exists() && caches[2].exists());
    assert!(run.cancelled && r.cancelled);
    assert_eq!(journal.inner.0.len(), 1, "a not-run item is never journaled");
}

#[test]
fn a_cancel_during_the_location_probe_never_becomes_the_fallback() {
    if common::unsupported("a_cancel_during_the_location_probe_never_becomes_the_fallback") {
        return;
    }
    // A probe killed by the forwarded signal fails like a location mismatch;
    // the cancel must stop the item, not delete the cache through the fallback.
    for probe_succeeds in [true, false] {
        let fx = Fx::new();
        let uv = fx.t.dir(".cache/uv");
        fx.t.file(".cache/uv/wheels/a.whl", 1000);
        let reviewed = fx.scan();
        let items = vec![listed(&reviewed, &uv)];
        assert_eq!(items[0].action, ActionKind::Native);
        let flag = Arc::new(AtomicBool::new(false));
        let set = Arc::clone(&flag);
        let commands = FakeCommands {
            on_run: Some(Box::new(move || set.store(true, Ordering::SeqCst))),
            ..FakeCommands::default().with("uv", probe_succeeds, &format!("{}\n", uv.display()))
        };
        let mut journal = MemoryJournal::default();

        let (_, r) = fx.clean(&items, &commands, &mut journal, &flag);

        assert_eq!(outcome(&r, &uv), &Outcome::NotRun, "probe succeeded: {probe_succeeds}");
        assert!(uv.join("wheels/a.whl").exists(), "nothing deleted");
        assert!(!fx.trash().0.exists(), "nothing trashed");
        assert_eq!(*commands.calls.borrow(), ["uv cache dir"], "only the probe ran");
        assert!(journal.0.is_empty());
    }
}

#[test]
fn a_user_data_item_is_refused_and_a_swapped_one_is_changed_since_scan() {
    if common::unsupported("a_user_data_item_is_refused_and_a_swapped_one_is_changed_since_scan") {
        return;
    }
    let fx = Fx::new();
    let pictures = fx.t.dir("Pictures");
    fx.t.file("Pictures/a.jpg", 1000);
    let cache = fx.pycache("q");
    let reviewed = fx.scan();
    let items = vec![listed(&reviewed, &pictures), listed(&reviewed, &cache)];
    assert_eq!(items[0].class, RiskClass::UserData);
    fs::rename(&cache, fx.t.path("q/old")).unwrap();
    fx.pycache("q");

    let (_, r) = fx.clean(&items, &FakeCommands::default(), &mut MemoryJournal::default(), &AtomicBool::new(false));

    assert_eq!(outcome(&r, &pictures), &Outcome::Skipped(SkipReason::ActionNotAllowed));
    assert_eq!(outcome(&r, &cache), &Outcome::Skipped(SkipReason::ChangedSinceScan));
    assert!(pictures.join("a.jpg").exists() && cache.exists());
}

#[test]
fn the_dry_run_predicts_and_changes_nothing() {
    if common::unsupported("the_dry_run_predicts_and_changes_nothing") {
        return;
    }
    let fx = Fx::new();
    let cache = fx.pycache("q");
    let modules = fx.review_modules("x");
    let spotify = fx.t.dir("Library/Caches/com.spotify.client");
    fx.t.file("Library/Caches/com.spotify.client/data", 1000);
    let uv = fx.t.dir(".cache/uv");
    fx.t.file(".cache/uv/wheels/a.whl", 1000);
    let reviewed = fx.scan();
    let items: Vec<ListedItem> = [&cache, &modules, &spotify, &uv].iter().map(|p| listed(&reviewed, p)).collect();
    assert_eq!(items[3].action, ActionKind::Native);
    let before = fx.snapshot();
    let commands = FakeCommands::default().with("uv", true, &format!("{}\n", uv.display()));
    let procs = FakeProcs::new(vec![app_process(4242, "/Applications/Spotify.app/Contents/MacOS/Spotify")]);

    let lines = fx.preflight(&items, &procs, &commands);

    let predictions: Vec<&Prediction> = lines
        .iter()
        .filter_map(|l| match l {
            PreflightLine::Item(i) => Some(&i.prediction),
            PreflightLine::Summary(_) => None,
        })
        .collect();
    assert_eq!(
        predictions,
        [
            &Prediction::WouldDelete,
            &Prediction::WouldTrash,
            &Prediction::WouldSkip(SkipReason::OwnerRunning { pid: 4242, name: "Spotify".into() }),
            &Prediction::WouldRun,
        ]
    );
    assert!(matches!(lines.last(), Some(PreflightLine::Summary(s)) if s.items == 4 && s.would_skip == 1));
    assert_eq!(fx.snapshot(), before, "tree identical: paths, kinds, inodes");
    assert!(commands.calls.borrow().is_empty(), "the tool was looked up, never run, not even its probe");
    assert!(!fx.data.path().join("logs").exists(), "no journal");
    assert!(!fx.trash().0.exists(), "nothing trashed");
}

#[test]
fn a_newly_kept_item_and_an_unknown_rule_get_the_same_reason_from_the_dry_run_and_the_clean() {
    if common::unsupported("a_newly_kept_item_and_an_unknown_rule_get_the_same_reason_from_the_dry_run_and_the_clean") {
        return;
    }
    let fx = Fx::new();
    let inside = fx.pycache("a");
    let below = fx.pycache("b");
    let unknown = fx.pycache("c");
    let reviewed = fx.scan();
    let mut items: Vec<ListedItem> = [&inside, &below, &unknown].iter().map(|p| listed(&reviewed, p)).collect();
    items[2].rule = "python.no-such-rule".into();
    // Kept after the review: a marker in the item, and one in the folder above another.
    fx.t.write("a/__pycache__/.ddc-keep", "");
    fx.t.write("b/.ddc-keep", "");
    let commands = FakeCommands::default();
    let expected = [SkipReason::Kept, SkipReason::Kept, SkipReason::RuleMismatch];

    let predicted: Vec<Prediction> = fx
        .preflight(&items, &FakeProcs::new(vec![]), &commands)
        .into_iter()
        .filter_map(|l| match l {
            PreflightLine::Item(i) => Some(i.prediction),
            PreflightLine::Summary(_) => None,
        })
        .collect();
    let (_, r) = fx.clean(&items, &commands, &mut MemoryJournal::default(), &AtomicBool::new(false));

    assert_eq!(predicted, expected.clone().map(Prediction::WouldSkip));
    let outcomes: Vec<&Outcome> = r.items.iter().map(|i| &i.outcome).collect();
    assert_eq!(outcomes, expected.each_ref().map(|e| Outcome::Skipped(e.clone())).each_ref());
    assert!(inside.exists() && below.exists() && unknown.exists());
}

/// Records every directory the walk opens, interleaved with the dry run's lines.
struct Counting {
    events: Arc<Mutex<Vec<String>>>,
}

impl DirReader for Counting {
    fn read(
        &self,
        dir: &Path,
        expect: Option<(u64, u64)>,
        want_private: bool,
        times: Times,
    ) -> Result<DirListing, ReadError> {
        self.events.lock().unwrap().push(format!("open {}", dir.display()));
        StdReader.read(dir, expect, want_private, times)
    }
}

#[test]
fn the_dry_run_walks_only_the_listed_items_and_streams_a_line_per_item() {
    if common::unsupported("the_dry_run_walks_only_the_listed_items_and_streams_a_line_per_item") {
        return;
    }
    let fx = Fx::new();
    let listed_paths = [fx.pycache("one"), fx.pycache("two"), fx.pycache("three")];
    // Unlisted trees beside and around them.
    for rel in ["big/a/b/c/d.bin", "one/src/lib/x.py", "two/docs/guide/y.md", "Music/album/z.mp3"] {
        fx.t.file(rel, 100);
    }
    fx.pycache("unlisted");
    let reviewed = fx.scan();
    let items: Vec<ListedItem> = listed_paths.iter().map(|p| listed(&reviewed, p)).collect();
    let events = Arc::new(Mutex::new(Vec::new()));
    let reader = Counting { events: Arc::clone(&events) };
    let commands = FakeCommands::default();
    let ports = PreflightPorts { commands: &commands, procs: &FakeProcs::new(vec![]), reader: &reader };

    let mut kinds = Vec::new();
    preflight::preflight(&fx.env(), &fx.catalog(), &[], &[], &items, &ports, &mut |line| {
        let tag = match line {
            PreflightLine::Item(i) => format!("line {}", i.path.display()),
            PreflightLine::Summary(_) => "summary".to_owned(),
        };
        kinds.push(tag.clone());
        events.lock().unwrap().push(tag);
        Ok(())
    })
    .unwrap();

    let events = events.lock().unwrap().clone();
    let opened: Vec<&str> = events.iter().filter_map(|e| e.strip_prefix("open ")).collect();
    assert!(!opened.is_empty());
    // Single-directory probes outside the items (the parent and project root for
    // lock files, ancestors for `.ddc-keep` and `.git`) do not go through the
    // walker's reader; every directory the walk opened lies inside a listed item.
    for dir in &opened {
        assert!(listed_paths.iter().any(|l| Path::new(dir).starts_with(l)), "walked outside the list: {dir}");
    }
    assert_eq!(kinds.len(), listed_paths.len() + 1);
    assert_eq!(kinds.last().unwrap(), "summary");
    // Each line is written as its item's checks finish: before the next item is walked.
    for (i, path) in listed_paths.iter().enumerate().skip(1) {
        let first_open = events.iter().position(|e| e == &format!("open {}", path.display())).unwrap();
        let previous_line =
            events.iter().position(|e| e == &format!("line {}", listed_paths[i - 1].display())).unwrap();
        assert!(previous_line < first_open, "{events:#?}");
    }
}

#[test]
fn a_malformed_list_is_refused_before_anything_runs() {
    let ok = r#"{"schema_version":1,"items":[{"path":"/Users/a/p/node_modules","dev":1,"ino":2,"rule":"node.node_modules","class":"rebuildable","action":"delete"}]}"#;
    assert!(ListedItems::parse(ok.as_bytes()).is_ok());
    let item = |path: &str, class: &str, action: &str, extra: &str| {
        format!(
            r#"{{"schema_version":1,"items":[{{"path":"{path}","dev":1,"ino":2,"rule":"r","class":"{class}","action":"{action}"{extra}}}]}}"#
        )
    };
    let cases = [
        (ok.replace("\"schema_version\":1", "\"schema_version\":2"), ListError::Version(2)),
        (r#"{"schema_version":1,"items":[]}"#.to_owned(), ListError::Empty),
        (item("relative/p", "safe", "delete", ""), ListError::Path("relative/p".into())),
        (item("/a/../p", "safe", "delete", ""), ListError::Path("/a/../p".into())),
        (item("/a/./p", "safe", "delete", ""), ListError::Path("/a/./p".into())),
        (item("/a//p", "safe", "delete", ""), ListError::Path("/a//p".into())),
        (item("/a/p/", "safe", "delete", ""), ListError::Path("/a/p/".into())),
        (item("/a/p", "safe", "delete", r#","permanent":true"#), ListError::Permanent("/a/p".into())),
        (item("/a/p", "review", "delete", ""), ListError::Permanent("/a/p".into())),
        (item("/a/p", "review", "trash", r#","permanent":true"#), ListError::Permanent("/a/p".into())),
    ];
    for (json, want) in cases {
        assert_eq!(ListedItems::parse(json.as_bytes()).unwrap_err(), want, "{json}");
    }
    let twice = ok.replace(
        "}]}",
        r#"},{"path":"/Users/a/p/node_modules","dev":1,"ino":2,"rule":"r","class":"safe","action":"delete"}]}"#,
    );
    assert_eq!(
        ListedItems::parse(twice.as_bytes()).unwrap_err(),
        ListError::Duplicate("/Users/a/p/node_modules".into())
    );
    let unknown = ok.replace("\"action\":\"delete\"", "\"action\":\"delete\",\"selected\":true");
    assert!(matches!(ListedItems::parse(unknown.as_bytes()), Err(ListError::Syntax(_))), "unknown fields are refused");
    let permanent_review = item("/a/p", "review", "delete", r#","permanent":true"#);
    assert!(ListedItems::parse(permanent_review.as_bytes()).is_ok());
}

#[test]
fn the_clean_report_round_trips_and_names_each_volume() {
    if common::unsupported("the_clean_report_round_trips_and_names_each_volume") {
        return;
    }
    let fx = Fx::new();
    let cache = fx.pycache("q");
    let reviewed = fx.scan();
    let items = vec![listed(&reviewed, &cache)];

    let (_, r) = fx.clean(&items, &FakeCommands::default(), &mut MemoryJournal::default(), &AtomicBool::new(false));

    let json = serde_json::to_value(&r).unwrap();
    assert_eq!(serde_json::from_value::<CleanReport>(json.clone()).unwrap(), r);
    assert_eq!(json["items"][0]["outcome"], "deleted");
    assert_eq!(r.volumes.len(), 1, "{:?}", r.volumes);
    assert!(r.volumes[0].free_after > 0);
}

#[test]
#[allow(clippy::disallowed_methods)] // DDC_UPDATE_SCHEMA regenerates the committed file
fn committed_clean_schema_is_current() {
    let mut generator = schemars::SchemaGenerator::default();
    // The list the app sends and the dry run's lines are part of the same contract.
    generator.subschema_for::<ListedItems>();
    generator.subschema_for::<PreflightLine>();
    let schema = generator.into_root_schema_for::<CleanReport>();
    let text = serde_json::to_string_pretty(&schema).unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/clean.schema.json");
    if std::env::var_os("DDC_UPDATE_SCHEMA").is_some() {
        fs::write(&path, &text).unwrap();
    }
    let committed = fs::read_to_string(&path).unwrap_or_default();
    assert!(committed == text, "docs/clean.schema.json is stale: run DDC_UPDATE_SCHEMA=1 cargo test");
}
