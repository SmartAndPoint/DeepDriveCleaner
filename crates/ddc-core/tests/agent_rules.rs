//! Story "Harness data rules" (epic DDC-003), its second step: the coding
//! agents' rules of ADR "Agent footprint: worktrees, harness data and
//! bounded consent for agents", §3. Fake harness folders in a temporary
//! home, never the real ones; the Trash and the process table are fakes.
//! A session folder's sessions are its entries (`tests/session_entries.rs`);
//! the folder itself stays report only. Codex, opencode and Cursor's
//! folders go to the Trash while their harness is not live
//! (`tests/harness_clean.rs`).

mod common;

use std::path::Path;
use std::time::{Duration, SystemTime};

use common::TreeBuilder;
use common::fakes::{FakeCommands, FakeProcs, FakeTrash};
use common::fixtures::rule_fixture;
use ddc_core::agents::liveness::UnknownCause;
use ddc_core::class::RiskClass;
use ddc_core::env::Env;
use ddc_core::exec::preflight::{self, Prediction, PreflightLine, PreflightPorts};
use ddc_core::exec::{self, Consent, ExecError, ExecOptions, ExecReport, MemoryJournal, Outcome, Ports, SkipReason};
use ddc_core::model::{Finding, Origin, OriginHarness, OriginKind, RemovalKind, ScanReport};
use ddc_core::plan::{Action, ActionKind, ListedItem, Plan, PlanItem, PlanOptions, build_plan, plan_listed};
use ddc_core::report::contents::EntryIndex;
use ddc_core::report::volumes::SystemStatFs;
use ddc_core::report::{self, Processes, ScanOptions};
use ddc_core::rules::entries::EntryLock;
use ddc_core::rules::{Catalog, RULES};
use ddc_core::scan::{Progress, StdReader, WalkOptions};

const HOUR: Duration = Duration::from_secs(3_600);

/// A session id, as Claude Code names a transcript.
const SESSION: &str = "0b5c3d2e-1f4a-4b6c-9d8e-7a6b5c4d3e2f";
/// A project folder of Claude Code's, named after `/Users/example/proj`.
const PROJECT: &str = ".claude/projects/-Users-example-proj";

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

    fn scan(&self) -> (ScanReport, EntryIndex) {
        let opts = ScanOptions {
            walk: WalkOptions { threads: Some(4), ..Default::default() },
            processes: Processes::fixed(FakeProcs::new(vec![])),
            ..Default::default()
        };
        report::scan_with_entries(&self.env(), &opts, &Progress::default(), &SystemStatFs).unwrap()
    }

    fn finding<'a>(&self, r: &'a ScanReport, rel: &str) -> Option<&'a Finding> {
        r.findings.iter().find(|f| f.path == self.t.path(rel))
    }

    fn summary(&self, r: &ScanReport) -> Vec<(String, String)> {
        r.findings
            .iter()
            .map(|f| (f.path.strip_prefix(self.t.root()).unwrap().display().to_string(), f.rule.clone()))
            .collect()
    }

    /// No finding at `rel` or under it.
    fn nothing_at_or_under(&self, r: &ScanReport, rel: &str) {
        let at = self.t.path(rel);
        let found: Vec<_> = r.findings.iter().filter(|f| f.path.starts_with(&at)).map(|f| &f.path).collect();
        assert!(found.is_empty(), "{rel}: {found:?}");
    }

    fn run(&self, plan: &Plan, consent: Consent) -> Result<ExecReport, ExecError> {
        let trash = FakeTrash(self.data.path().join("FakeTrash"));
        let mut journal = MemoryJournal::default();
        let procs = FakeProcs::new(vec![]);
        exec::execute(
            &self.env(),
            &self.catalog(),
            plan,
            &consent,
            Ports { commands: &FakeCommands::default(), trash: &trash, procs: &procs, journal: &mut journal },
            &ExecOptions::default(),
        )
    }

    /// A finding as the app lists it.
    fn listed(&self, r: &ScanReport, rel: &str, action: ActionKind) -> ListedItem {
        let f = self.finding(r, rel).unwrap_or_else(|| panic!("no finding at {rel}: {:?}", self.summary(r)));
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
}

fn outcome<'a>(r: &'a ExecReport, path: &Path) -> &'a Outcome {
    &r.items.iter().find(|i| i.path == path).unwrap_or_else(|| panic!("{path:?} did not run: {:#?}", r.items)).outcome
}

fn yes() -> Consent {
    Consent::AssumeYes { classes: vec![RiskClass::Safe, RiskClass::Rebuildable] }
}

/// AC 1: each path of the eight rules is found with its id, class, method
/// and `origin`; opencode's `auth.json` and `opencode.db` with its `-wal`
/// and `-shm` are in no finding.
#[test]
fn each_path_of_the_eight_rules_is_found_with_its_rule_and_origin() {
    if common::unsupported("each_path_of_the_eight_rules_is_found_with_its_rule_and_origin") {
        return;
    }
    use OriginHarness::*;
    use OriginKind::*;
    use RemovalKind::{Delete, Trash};
    use RiskClass::{Review, Safe};
    let none = RemovalKind::None;
    let h = Home::new();
    #[rustfmt::skip]
    let table: &[(&str, &str, RiskClass, RemovalKind, OriginHarness, OriginKind)] = &[
        (".claude/debug", "agent.claude-code-scratch", Safe, Delete, ClaudeCode, RuntimeCache),
        (".claude/todos", "agent.claude-code-scratch", Safe, Delete, ClaudeCode, RuntimeCache),
        (".claude/statsig", "agent.claude-code-scratch", Safe, Delete, ClaudeCode, RuntimeCache),
        (".claude/logs", "agent.claude-code-scratch", Safe, Delete, ClaudeCode, RuntimeCache),
        (".claude/file-history", "agent.claude-code-file-history", Review, none, ClaudeCode, SessionData),
        (PROJECT, "agent.claude-code-transcripts", Review, none, ClaudeCode, SessionData),
        (".claude/paste-cache", "agent.claude-code-history", Review, Trash, ClaudeCode, SessionData),
        (".claude/usage-data", "agent.claude-code-history", Review, Trash, ClaudeCode, SessionData),
        // To the Trash while the harness is not live.
        (".codex/sessions", "agent.codex-sessions", Review, Trash, Codex, SessionData),
        (".codex/log", "agent.codex-sessions", Review, Trash, Codex, SessionData),
        (".cache/codex-runtimes", "agent.codex-runtimes", Review, Trash, Codex, RuntimeCache),
        (".local/share/opencode/storage", "agent.opencode-data", Review, Trash, Opencode, SessionData),
        (".local/share/opencode/snapshot", "agent.opencode-data", Review, Trash, Opencode, SessionData),
        (".local/share/opencode/log", "agent.opencode-data", Review, Trash, Opencode, SessionData),
        (".cache/opencode", "agent.opencode-data", Review, Trash, Opencode, SessionData),
        (".cursor/projects", "agent.cursor-agent-data", Review, Trash, Cursor, SessionData),
        (".cursor/ai-tracking", "agent.cursor-agent-data", Review, Trash, Cursor, SessionData),
    ];
    for (rel, ..) in table {
        h.t.file(&format!("{rel}/payload"), 1_000);
    }
    h.t.file(&format!("{PROJECT}/{SESSION}.jsonl"), 1_000);
    h.t.write(&format!("{PROJECT}/memory/MEMORY.md"), "- a note\n");
    let secrets = ["auth.json", "opencode.db", "opencode.db-wal", "opencode.db-shm"];
    for name in secrets {
        h.t.file(&format!(".local/share/opencode/{name}"), 1_000);
    }

    let (r, _) = h.scan();

    for &(rel, id, class, removal, harness, kind) in table {
        let f = h.finding(&r, rel).unwrap_or_else(|| panic!("no finding at {rel}: {:?}", h.summary(&r)));
        assert_eq!((f.rule.as_str(), f.class, f.removal), (id, class, removal), "{rel}");
        assert_eq!(f.origin, Some(Origin { harness, kind }), "{rel}");
        assert_eq!(f.selected, class == Safe, "{rel}: only the scratch is selected by default");
        let rule = RULES.iter().find(|r| r.id == id).unwrap();
        assert_eq!(rule.listed_only, class == Review, "{id}: only a person removes the review rules");
        assert!(rule.owners.is_empty(), "{id}: no owner by process name");
    }
    let agents: Vec<_> = r.findings.iter().filter(|f| f.rule.starts_with("agent.")).collect();
    assert_eq!(agents.len(), table.len(), "{:?}", h.summary(&r));
    // The session folder is the finding, its session its one entry; with no
    // records folder, whether it is live cannot be told.
    let contents = h.finding(&r, PROJECT).unwrap().contents.as_ref().unwrap();
    let entry = &contents.entries[..];
    assert_eq!(entry.len(), 1, "{entry:?}");
    assert_eq!(entry[0].path, h.t.path(&format!("{PROJECT}/{SESSION}.jsonl")));
    assert_eq!(entry[0].lock, Some(EntryLock::LivenessUnknown { cause: UnknownCause::RecordsMissing }));
    assert_eq!(contents.rest_count, 2, "`payload` and `memory/`");
    for name in secrets {
        let file = h.t.path(&format!(".local/share/opencode/{name}"));
        let holder: Vec<_> = r.findings.iter().filter(|f| file.starts_with(&f.path)).map(|f| &f.path).collect();
        assert!(holder.is_empty(), "{name} is in {holder:?}");
    }
    assert!(h.finding(&r, ".local/share/opencode").is_none());
}

/// AC 2, apart from entries: auto memory, the live records, the shell
/// snapshots, what the person installed and their plans and chats are
/// never findings; the folders holding worktrees are none either, a
/// worktree is a git finding and a `node_modules` inside one still is one;
/// Cursor's desktop caches stay
/// `ide.cursor-cache`; a `.venv` in a session's tool results is no venv; a
/// list naming `memory/` or the project folder itself is refused.
#[test]
fn sealed_folders_and_auto_memory_are_never_findings() {
    if common::unsupported("sealed_folders_and_auto_memory_are_never_findings") {
        return;
    }
    let h = Home::new();
    let t = &h.t;
    t.file(&format!("{PROJECT}/{SESSION}.jsonl"), 1_000);
    t.write(&format!("{PROJECT}/sessions-index.json"), "{}");
    t.write(&format!("{PROJECT}/memory/MEMORY.md"), "- a note\n");
    t.write(&format!("{PROJECT}/memory/package.json"), "{}");
    t.file(&format!("{PROJECT}/memory/node_modules/x.js"), 1_000);
    t.write(&format!("{PROJECT}/{SESSION}/tool-results/.venv/pyvenv.cfg"), "home = /usr/bin\n");
    t.file(&format!("{PROJECT}/{SESSION}/tool-results/.venv/lib/x.py"), 1_000);
    t.write(".claude/sessions/4242.json", "{}");
    t.write(".claude/shell-snapshots/snapshot-zsh-1.sh", "export A=1\n");
    let project = |rel: &str| {
        t.write(&format!("{rel}/package.json"), "{}");
        t.write(&format!("{rel}/package-lock.json"), "{}");
        t.file(&format!("{rel}/node_modules/dep/index.js"), 1_000);
    };
    project(".claude/sessions");
    project(".claude/shell-snapshots");
    project(".claude/plugins/marketplaces/m");
    t.write(".claude/plugins/cache/p/Cargo.toml", "");
    t.file(".claude/plugins/cache/p/target/debug/x", 1_000);
    project(".cursor/extensions/vendor.ext-1.0.0");
    project(".cursor/plugins/p");
    t.write(".cursor/plans/plan.md", "# plan\n");
    t.write(".cursor/chats/chat.json", "{}");
    t.write(".codex/worktrees/w1/.git", "gitdir: /Users/example/repo/.git/worktrees/w1\n");
    project(".codex/worktrees/w1");
    t.write(".cursor/worktrees/repo/w2/.git", "gitdir: /Users/example/repo/.git/worktrees/w2\n");
    project(".cursor/worktrees/repo/w2");
    t.file(&format!("{}/data_0", common::rule_path("ide.cursor-cache")), 1_000);

    let (r, entries) = h.scan();

    for sealed in [
        ".claude/sessions",
        ".claude/shell-snapshots",
        ".claude/plugins",
        ".cursor/extensions",
        ".cursor/plugins",
        ".cursor/plans",
        ".cursor/chats",
        &format!("{PROJECT}/memory"),
    ] {
        h.nothing_at_or_under(&r, sealed);
    }
    for folder in [".codex/worktrees", ".cursor/worktrees", ".cursor/worktrees/repo"] {
        assert!(h.finding(&r, folder).is_none(), "{folder}: {:?}", h.summary(&r));
    }
    // A worktree itself is a git finding, no agent rule's (tests/worktrees.rs);
    // here its repository is absent, so an orphan.
    for worktree in [".codex/worktrees/w1", ".cursor/worktrees/repo/w2"] {
        let f = h.finding(&r, worktree).unwrap_or_else(|| panic!("no finding at {worktree}: {:?}", h.summary(&r)));
        assert_eq!(f.rule, "git.worktree-orphan", "{worktree}");
    }
    for modules in [".codex/worktrees/w1/node_modules", ".cursor/worktrees/repo/w2/node_modules"] {
        let f = h.finding(&r, modules).unwrap_or_else(|| panic!("no finding at {modules}: {:?}", h.summary(&r)));
        assert_eq!(f.rule, "node.node_modules", "{modules}");
    }
    assert_eq!(h.finding(&r, common::rule_path("ide.cursor-cache")).unwrap().rule, "ide.cursor-cache");
    assert!(r.findings.iter().all(|f| f.rule != "python.venv"), "{:?}", h.summary(&r));
    let folder = h.finding(&r, PROJECT).unwrap();
    assert_eq!(folder.rule, "agent.claude-code-transcripts");

    // `memory/`, as the app could list it, whichever way: never planned.
    let memory = t.path(&format!("{PROJECT}/memory"));
    let (dev, ino) = common::id(&memory);
    let as_entry = ListedItem {
        path: memory.clone(),
        dev,
        ino,
        rule: "agent.claude-code-transcripts".to_owned(),
        class: RiskClass::UserData,
        action: ActionKind::Trash,
        permanent: false,
    };
    let as_finding = ListedItem { class: RiskClass::Review, ..as_entry.clone() };
    // The project folder whole: its method is `none`.
    let whole = h.listed(&r, PROJECT, ActionKind::Trash);
    let items = [as_entry, as_finding, whole];
    let planned = plan_listed(&r, &entries, &h.catalog(), &items);
    assert!(planned.plan.items.is_empty(), "{:?}", planned.plan.items);
    assert_eq!(planned.refused.len(), 3);
    let refused = |path: &Path| -> Vec<SkipReason> {
        planned.refused.iter().filter(|x| x.item.path == path).map(|x| x.reason.clone()).collect()
    };
    // No entry of the session folder, and no finding of the scan.
    assert_eq!(refused(&memory), [SkipReason::NotAnEntry, SkipReason::ChangedSinceScan]);
    assert_eq!(refused(&t.path(PROJECT)), [SkipReason::ActionNotAllowed]);
    // The dry run gives each the plan's reason.
    let mut predicted = Vec::new();
    let ports =
        PreflightPorts { commands: &FakeCommands::default(), procs: &FakeProcs::new(vec![]), reader: &StdReader };
    preflight::preflight(&h.env(), &h.catalog(), &[], &[], &items, &ports, &mut |line| {
        if let PreflightLine::Item(i) = line {
            predicted.push(i.prediction.clone());
        }
        Ok(())
    })
    .unwrap();
    assert_eq!(
        predicted,
        [SkipReason::NotAnEntry, SkipReason::ChangedSinceScan, SkipReason::ActionNotAllowed].map(Prediction::WouldSkip)
    );
}

/// AC 6: `agent.claude-code-history` is not in a `--yes` plan, and
/// `paste-cache` still exists after the clean, while the scratch beside it
/// is deleted.
#[test]
fn history_is_never_in_a_yes_plan_and_outlives_the_clean() {
    if common::unsupported("history_is_never_in_a_yes_plan_and_outlives_the_clean") {
        return;
    }
    let h = Home::new();
    h.t.file(".claude/paste-cache/0123abcd.txt", 1_000);
    h.t.file(".claude/usage-data/report.html", 1_000);
    h.t.file(".claude/debug/latest.txt", 1_000);
    let (r, _) = h.scan();
    let plan = build_plan(&r, &h.catalog(), &PlanOptions::default());
    assert!(plan.items.iter().all(|i| i.finding.rule != "agent.claude-code-history"), "{:?}", plan.items);

    let run = h.run(&plan, yes()).unwrap();

    assert!(h.t.path(".claude/paste-cache/0123abcd.txt").exists());
    assert!(h.t.path(".claude/usage-data/report.html").exists());
    assert_eq!(outcome(&run, &h.t.path(".claude/debug")), &Outcome::Deleted);
    assert!(run.items.iter().all(|i| i.rule != "agent.claude-code-history"));
}

/// AC 6: `--yes --class review` is refused `review-with-yes`, as for any
/// review item, before anything runs.
#[test]
fn yes_with_the_review_class_is_refused() {
    if common::unsupported("yes_with_the_review_class_is_refused") {
        return;
    }
    let h = Home::new();
    h.t.file(".claude/paste-cache/0123abcd.txt", 1_000);
    let (r, _) = h.scan();
    let f = h.finding(&r, ".claude/paste-cache").unwrap().clone();
    let plan = Plan {
        roots: r.scan_roots.clone(),
        items: vec![PlanItem { finding: f, action: Action::Trash, default_selected: true, selected: true }],
    };

    let err = h.run(&plan, Consent::AssumeYes { classes: vec![RiskClass::Review] }).unwrap_err();

    assert!(matches!(err, ExecError::ReviewWithYes), "{err:?}");
    assert!(h.t.path(".claude/paste-cache/0123abcd.txt").exists());
}

/// AC 6: a plan built without a list never holds a `listed_only` rule's
/// item, whatever the classes and `--permanent`; the executor refuses one
/// a plan holds under any consent but a list, and a list moves it to the
/// Trash at once.
#[test]
fn a_plan_without_a_list_never_holds_a_listed_only_item() {
    if common::unsupported("a_plan_without_a_list_never_holds_a_listed_only_item") {
        return;
    }
    let h = Home::new();
    let listed_only: Vec<_> = RULES.iter().filter(|r| r.listed_only).collect();
    assert!(!listed_only.is_empty());
    for rule in &listed_only {
        rule_fixture(&h.t, rule).unwrap_or_else(|| panic!("{} has no finding to plant", rule.id));
    }
    let (r, entries) = h.scan();
    for rule in &listed_only {
        assert!(r.findings.iter().any(|f| f.rule == rule.id), "{}: {:?}", rule.id, h.summary(&r));
    }
    let every = vec![RiskClass::Safe, RiskClass::Rebuildable, RiskClass::Review, RiskClass::UserData];
    for opts in [
        PlanOptions::default(),
        PlanOptions { classes: every.clone(), ..Default::default() },
        PlanOptions { classes: vec![RiskClass::Review], ..Default::default() },
        PlanOptions { classes: every, permanent: true, ..Default::default() },
    ] {
        let plan = build_plan(&r, &h.catalog(), &opts);
        let held: Vec<_> = plan.items.iter().filter(|i| listed_only.iter().any(|r| r.id == i.finding.rule)).collect();
        assert!(held.is_empty(), "{opts:?}: {held:?}");
    }

    // A plan made by hand, as a person at the terminal would confirm it.
    let paste = h.t.path(".claude/paste-cache");
    let f = r.findings.iter().find(|f| f.path == paste).unwrap().clone();
    let item = PlanItem { finding: f, action: Action::Trash, default_selected: true, selected: true };
    let by_hand = Plan { roots: r.scan_roots.clone(), items: vec![item] };
    for consent in [Consent::Interactive, yes()] {
        let run = h.run(&by_hand, consent.clone()).unwrap();
        assert!(matches!(outcome(&run, &paste), Outcome::Skipped(_)), "{consent:?}: {:?}", run.items);
        assert!(paste.exists(), "{consent:?}");
    }
    let run = h.run(&by_hand, Consent::Interactive).unwrap();
    assert_eq!(outcome(&run, &paste), &Outcome::Skipped(SkipReason::ActionNotAllowed));

    // Listed by a person, it goes to the Trash at once.
    let listed = plan_listed(&r, &entries, &h.catalog(), &[h.listed(&r, ".claude/paste-cache", ActionKind::Trash)]);
    assert!(listed.refused.is_empty(), "{:?}", listed.refused);
    let run = h.run(&listed.plan, Consent::Listed).unwrap();
    assert_eq!(outcome(&run, &paste), &Outcome::Trashed);
    assert!(!paste.exists());
}

/// Every platform lists the same rules: the harnesses keep their folders
/// at the same home-relative paths everywhere.
#[test]
fn every_platform_lists_the_agent_rules() {
    use ddc_core::platform::Platform;
    use ddc_core::rules::rules_of;
    let agent =
        |p: Platform| rules_of(p).iter().filter(|r| r.id.starts_with("agent.")).map(|r| r.id).collect::<Vec<_>>();
    assert_eq!(agent(Platform::Macos).len(), 10);
    for p in [Platform::Linux, Platform::Windows] {
        assert_eq!(agent(p), agent(Platform::Macos), "{p}");
    }
}
