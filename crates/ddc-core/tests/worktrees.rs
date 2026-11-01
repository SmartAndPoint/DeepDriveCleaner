//! Story "Worktree findings and provenance" (epic DDC-003), its first step:
//! a linked git worktree is a finding, `git.worktree`, or
//! `git.worktree-orphan` when its repository is gone; a region, so the rules
//! keep working inside it; with its harness in `origin` and its state in
//! `worktree`. Read only: both rules are `review` and remove nothing yet.
//! Real repositories made with the `git` on `PATH` (`common::git`), in a
//! temporary home.

mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use common::fakes::FakeProcs;
use common::{OLD, TreeBuilder, git};
use ddc_core::class::{Category, RiskClass};
use ddc_core::env::Env;
use ddc_core::model::{
    Finding, NotSelected, Origin, OriginHarness, OriginKind, RemovalKind, ScanReport, WorktreeProbe,
};
use ddc_core::report::{self, Processes, ScanOptions};
use ddc_core::rules::{LockFiles, Note, RULES, effective_class};
use ddc_core::scan::{Progress, WalkOptions};

const HOUR: Duration = Duration::from_secs(3_600);

struct Home {
    t: TreeBuilder,
    data: tempfile::TempDir,
    now: SystemTime,
}

impl Home {
    /// A home, or `None` after printing why the test skips: the scanner is
    /// not ported here, or no git runs.
    fn ready(test: &str) -> Option<Self> {
        if common::unsupported(test) {
            return None;
        }
        common::skipped_without(test, git::available())?;
        Some(Self { t: TreeBuilder::new(), data: tempfile::tempdir().unwrap(), now: SystemTime::now() + HOUR })
    }

    fn env(&self) -> Env {
        Env::with_paths(self.t.root(), self.data.path(), &self.data.path().join("logs"), self.now).unwrap()
    }

    fn scan(&self) -> ScanReport {
        let opts = ScanOptions {
            walk: WalkOptions { threads: Some(4), ..Default::default() },
            processes: Processes::fixed(FakeProcs::new(vec![])),
            ..Default::default()
        };
        report::scan(&self.env(), &opts, &Progress::default()).unwrap()
    }

    fn summary(&self, r: &ScanReport) -> Vec<(String, String)> {
        r.findings
            .iter()
            .map(|f| (f.path.strip_prefix(self.t.root()).unwrap_or(&f.path).display().to_string(), f.rule.clone()))
            .collect()
    }

    fn finding<'a>(&self, r: &'a ScanReport, path: &Path) -> &'a Finding {
        r.findings
            .iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("no finding at {}: {:?}", path.display(), self.summary(r)))
    }

    fn none_at(&self, r: &ScanReport, path: &Path) {
        assert!(r.findings.iter().all(|f| f.path != path), "{}: {:?}", path.display(), self.summary(r));
    }

    /// `rel` relative to the home, for [`TreeBuilder::touch`].
    fn rel<'p>(&self, path: &'p Path) -> &'p str {
        path.strip_prefix(self.t.root()).unwrap().to_str().unwrap()
    }
}

fn origin(harness: OriginHarness) -> Option<Origin> {
    Some(Origin { harness, kind: OriginKind::Worktree })
}

/// `cp -r`: the folders and files under `from`, at `to`.
fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let target = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &target);
        } else {
            fs::copy(e.path(), &target).unwrap();
        }
    }
}

#[test]
fn a_claude_code_worktree_is_a_git_worktree_finding_and_its_main_checkout_none() {
    let Some(h) = Home::ready("a_claude_code_worktree_is_a_git_worktree_finding_and_its_main_checkout_none") else {
        return;
    };
    let main = git::repo(&h.t, "proj");
    let wt = git::worktree(&main, &main.join(".claude/worktrees/x"), &["-b", "worktree-x"]);

    let r = h.scan();

    h.none_at(&r, &main);
    let f = h.finding(&r, &wt);
    assert_eq!(f.rule, "git.worktree");
    assert_eq!(f.origin, origin(OriginHarness::ClaudeCode));
    assert_eq!((f.category, f.class, f.removal), (Category::ProjectArtifact, RiskClass::Review, RemovalKind::None));
    assert_eq!((f.selected, f.not_selected), (false, Some(NotSelected::Review)));
    let project = f.project.as_ref().expect("a project");
    assert_eq!(project.root, main, "a worktree's project is its main checkout");
    assert_eq!(project.worktree_of, None);
    let state = f.worktree.as_ref().expect("a worktree state");
    assert!(state.linked);
    assert_eq!((state.branch.as_deref(), state.detached, state.locked), (Some("worktree-x"), Some(false), Some(false)));
    assert_eq!(state.probe, WorktreeProbe::Unknown, "no git probe runs before the story's next step");
    assert_eq!((state.clean, state.own_commits, state.submodules, state.in_use), (None, None, None, None));
    assert!(f.hint.starts_with(&format!("a linked worktree of {};", main.display())), "{}", f.hint);
    assert!(f.hint.contains("git worktree remove"), "{}", f.hint);
    assert_eq!(r.findings.iter().filter(|f| f.rule.starts_with("git.")).count(), 1, "{:?}", h.summary(&r));
}

#[test]
fn a_worktree_added_beside_the_checkout_is_the_persons_own() {
    let Some(h) = Home::ready("a_worktree_added_beside_the_checkout_is_the_persons_own") else { return };
    let main = git::repo(&h.t, "proj");
    let wt = git::worktree(&main, &h.t.path("y"), &[]);

    let r = h.scan();

    let f = h.finding(&r, &wt);
    assert_eq!(f.rule, "git.worktree");
    assert_eq!(f.origin, origin(OriginHarness::Git));
    assert_eq!(f.project.as_ref().unwrap().root, main);
    assert_eq!(f.worktree.as_ref().unwrap().branch.as_deref(), Some("y"), "git names the new branch after the folder");
    assert!(f.hint.starts_with("yours: a linked worktree of"), "never a failed condition: {}", f.hint);
    h.none_at(&r, &main);
}

#[test]
fn the_gitdir_markers_tell_the_harness() {
    let Some(h) = Home::ready("the_gitdir_markers_tell_the_harness") else { return };
    let main = git::repo(&h.t, "proj");
    let head = git::git(&main, &["rev-parse", "HEAD"]);
    let mark = |name: &str, file: &str, content: &str| {
        let wt = git::worktree(&main, &h.t.path(&format!("elsewhere/{name}")), &[]);
        fs::write(git::gitdir(&wt).join(file), content).unwrap();
        wt
    };
    let cli = mark("cli", "CLAUDE_BASE", &head);
    let desktop = mark("desktop", "claude-desktop-worktree", "");
    let codex = mark("codex", "codex-thread.json", r#"{"ownerThreadId":"t","version":1}"#);
    let not_a_base = mark("plain", "CLAUDE_BASE", "not a commit");

    let r = h.scan();

    for (wt, harness) in [
        (&cli, OriginHarness::ClaudeCode),
        (&desktop, OriginHarness::ClaudeCode),
        (&codex, OriginHarness::Codex),
        (&not_a_base, OriginHarness::Git),
    ] {
        let f = h.finding(&r, wt);
        assert_eq!((f.rule.as_str(), f.origin), ("git.worktree", origin(harness)), "{}", wt.display());
    }
}

#[test]
fn a_git_file_naming_a_missing_gitdir_is_an_orphan() {
    let Some(h) = Home::ready("a_git_file_naming_a_missing_gitdir_is_an_orphan") else { return };
    let main = git::repo(&h.t, "repo");
    let wt = git::worktree(&main, &h.t.path("left"), &[]);
    let claude = git::worktree(&main, &main.join(".claude/worktrees/c"), &[]);
    // The repository goes, its worktrees stay: a copy of files whose `.git` names nothing.
    copy_tree(&claude, &h.t.path("saved/.claude/worktrees/c"));
    fs::rename(&main, h.t.path("moved-away")).unwrap();

    let r = h.scan();

    let f = h.finding(&r, &wt);
    assert_eq!(f.rule, "git.worktree-orphan");
    assert_eq!((f.class, f.removal, f.selected), (RiskClass::Review, RemovalKind::None, false));
    assert_eq!(f.origin, origin(OriginHarness::Git));
    assert_eq!(f.project, None, "its repository is gone");
    assert_eq!(f.worktree, None);
    assert!(f.hint.contains("repository is gone") && f.hint.contains("not mounted"), "{}", f.hint);
    let copied = h.finding(&r, &h.t.path("saved/.claude/worktrees/c"));
    assert_eq!((copied.rule.as_str(), copied.origin), ("git.worktree-orphan", origin(OriginHarness::ClaudeCode)));
    h.none_at(&r, &h.t.path("moved-away"));
}

#[test]
fn a_submodule_is_not_a_worktree() {
    let Some(h) = Home::ready("a_submodule_is_not_a_worktree") else { return };
    let lib = git::repo(&h.t, "lib");
    let main = git::repo(&h.t, "proj");
    git::git(&main, &["submodule", "add", "-q", lib.to_str().unwrap(), "libs/lib"]);
    assert!(fs::symlink_metadata(main.join("libs/lib/.git")).unwrap().is_file(), "a submodule's .git is a file");

    let r = h.scan();

    assert!(r.findings.iter().all(|f| !f.rule.starts_with("git.")), "{:?}", h.summary(&r));
}

#[test]
fn a_venv_inside_a_worktree_belongs_to_the_worktree_of_its_main_checkout() {
    let Some(h) = Home::ready("a_venv_inside_a_worktree_belongs_to_the_worktree_of_its_main_checkout") else {
        return;
    };
    let main = git::repo(&h.t, "proj");
    let wt = git::worktree(&main, &main.join(".claude/worktrees/x"), &[]);
    let rel = h.rel(&wt).to_owned();
    h.t.write(&format!("{rel}/uv.lock"), "");
    h.t.write(&format!("{rel}/.venv/pyvenv.cfg"), "home = /usr/bin\n");
    h.t.file(&format!("{rel}/.venv/lib/site.py"), 10_000);

    let r = h.scan();

    let venv = h.finding(&r, &wt.join(".venv"));
    assert_eq!((venv.rule.as_str(), venv.class), ("python.venv", RiskClass::Rebuildable));
    let project = venv.project.as_ref().unwrap();
    assert_eq!(project.root, wt, "the worktree is the venv's project");
    assert_eq!(project.worktree_of.as_deref(), Some(main.as_path()));
    assert_eq!(venv.origin, None, "the venv's own rule tells no harness");
    let worktree = h.finding(&r, &wt);
    assert_eq!(worktree.rule, "git.worktree", "still a region: rules fire inside it");
    assert!(worktree.bytes < venv.bytes, "the region's bytes leave out the venv: {} {}", worktree.bytes, venv.bytes);
}

/// Decision 9: a worktree's activity is its own, read through its gitdir,
/// never its main checkout's.
#[test]
fn a_worktrees_activity_is_its_own() {
    let Some(h) = Home::ready("a_worktrees_activity_is_its_own") else { return };
    let main = git::repo(&h.t, "proj");
    let wt = git::worktree(&main, &main.join(".claude/worktrees/x"), &[]);
    let gitdir = git::gitdir(&wt);
    for f in ["index", "HEAD"] {
        h.t.touch(h.rel(&gitdir.join(f)), OLD);
    }
    h.t.write(&format!("{}/uv.lock", h.rel(&wt)), "");
    h.t.touch(&format!("{}/uv.lock", h.rel(&wt)), OLD);
    h.t.touch(h.rel(&main.join(".git/index")), OLD + 86_400 * 365 * 2);

    let r = h.scan();

    let f = h.finding(&r, &wt);
    let project = f.project.as_ref().unwrap();
    assert_eq!(project.root, main);
    assert_eq!(project.last_activity.as_deref(), Some("2024-01-01T00:00:00Z"), "its gitdir's, not the main checkout's");
}

#[test]
fn a_worktree_copied_or_moved_by_hand_is_not_linked() {
    let Some(h) = Home::ready("a_worktree_copied_or_moved_by_hand_is_not_linked") else { return };
    let main = git::repo(&h.t, "proj");
    let wt = git::worktree(&main, &h.t.path("w"), &[]);
    let moving = git::worktree(&main, &h.t.path("m"), &[]);
    let copy = h.t.path("copy");
    copy_tree(&wt, &copy);
    let moved = h.t.path("moved");
    fs::rename(&moving, &moved).unwrap();

    let r = h.scan();

    assert!(h.finding(&r, &wt).worktree.as_ref().unwrap().linked, "the original");
    for path in [&copy, &moved] {
        let f = h.finding(&r, path);
        assert_eq!(f.rule, "git.worktree", "{}", path.display());
        assert!(!f.worktree.as_ref().unwrap().linked, "{}", path.display());
        assert!(f.hint.contains("git worktree repair"), "{}", f.hint);
        assert_eq!(f.removal, RemovalKind::None);
    }
}

#[test]
fn a_relative_back_pointer_is_linked() {
    let Some(h) = Home::ready("a_relative_back_pointer_is_linked") else { return };
    let main = git::repo(&h.t, "proj");
    let path = h.t.path("rel");
    if let Err(why) = git::try_git(&main, &["worktree", "add", "-q", "--relative-paths", path.to_str().unwrap()]) {
        // Before git 2.48: the same two files, written as that option writes them.
        eprintln!("note: this git makes no relative worktree ({why}); writing its pointers by hand");
        git::worktree(&main, &path, &[]);
        fs::write(git::gitdir(&path).join("gitdir"), "../../../../rel/.git\n").unwrap();
        fs::write(path.join(".git"), "gitdir: ../proj/.git/worktrees/rel\n").unwrap();
    }
    let back = fs::read_to_string(git::gitdir(&path).join("gitdir")).unwrap();
    assert!(!back.starts_with('/'), "a relative back-pointer: {back}");

    let r = h.scan();

    let f = h.finding(&r, &path);
    assert_eq!(f.rule, "git.worktree");
    assert!(f.worktree.as_ref().unwrap().linked);
    assert_eq!(f.project.as_ref().unwrap().root, main);
}

#[test]
fn a_locked_and_a_detached_worktree_say_so() {
    let Some(h) = Home::ready("a_locked_and_a_detached_worktree_say_so") else { return };
    let main = git::repo(&h.t, "proj");
    let locked = git::worktree(&main, &h.t.path("locked"), &[]);
    git::git(&main, &["worktree", "lock", "--reason", "kept", locked.to_str().unwrap()]);
    let detached = git::worktree(&main, &h.t.path("detached"), &["--detach"]);

    let r = h.scan();

    assert_eq!(h.finding(&r, &locked).worktree.as_ref().unwrap().locked, Some(true));
    let state = h.finding(&r, &detached).worktree.clone().unwrap();
    assert_eq!((state.branch, state.detached, state.locked), (None, Some(true), Some(false)));
}

#[test]
fn a_kept_worktree_is_one_kept_finding() {
    let Some(h) = Home::ready("a_kept_worktree_is_one_kept_finding") else { return };
    let main = git::repo(&h.t, "proj");
    let wt = git::worktree(&main, &h.t.path("w"), &[]);
    h.t.write("w/.ddc-keep", "");
    h.t.write("w/uv.lock", "");
    h.t.write("w/.venv/pyvenv.cfg", "");

    let r = h.scan();

    let f = h.finding(&r, &wt);
    assert_eq!(f.rule, "user.kept");
    assert_eq!(f.protection.as_ref().unwrap().rule.as_deref(), Some("git.worktree"));
    h.none_at(&r, &wt.join(".venv"));
}

/// Decision 5: an agent's worktree under a hidden folder of the home is a
/// project like any other; its artifacts are not `app-owned`.
#[test]
fn an_agents_worktree_under_a_hidden_folder_is_not_app_owned() {
    let Some(h) = Home::ready("an_agents_worktree_under_a_hidden_folder_is_not_app_owned") else { return };
    let main = git::repo(&h.t, "src/repo");
    let wt = git::worktree(&main, &h.t.path(".codex/worktrees/abcd/repo"), &[]);
    fs::write(git::gitdir(&wt).join("codex-thread.json"), "{}").unwrap();
    h.t.write(".codex/worktrees/abcd/repo/uv.lock", "");
    h.t.write(".codex/worktrees/abcd/repo/.venv/pyvenv.cfg", "");
    // The same venv outside any worktree is app-owned.
    h.t.write(".codex/other/uv.lock", "");
    h.t.write(".codex/other/.venv/pyvenv.cfg", "");

    let r = h.scan();

    let f = h.finding(&r, &wt);
    assert_eq!((f.rule.as_str(), f.origin), ("git.worktree", origin(OriginHarness::Codex)));
    let venv = h.finding(&r, &wt.join(".venv"));
    assert_eq!(venv.class, RiskClass::Rebuildable);
    assert!(!venv.notes.contains(&Note::AppOwned), "{:?}", venv.notes);
    let outside = h.finding(&r, &h.t.path(".codex/other/.venv"));
    assert_eq!((outside.class, outside.notes.as_slice()), (RiskClass::Review, &[Note::AppOwned][..]));

    // A listed item's class, without a walk, is the scan's.
    let rule = RULES.iter().find(|r| r.id == "python.venv").unwrap();
    let roots = [h.t.root()];
    let mut locks = LockFiles::default();
    assert_eq!(effective_class(rule, &wt.join(".venv"), &roots, h.t.root(), &mut locks), RiskClass::Rebuildable);
    let other = h.t.path(".codex/other/.venv");
    assert_eq!(effective_class(rule, &other, &roots, h.t.root(), &mut locks), RiskClass::Review);
}

#[test]
fn a_report_with_worktrees_holds_its_schema() {
    let Some(h) = Home::ready("a_report_with_worktrees_holds_its_schema") else { return };
    let main = git::repo(&h.t, "proj");
    let wt = git::worktree(&main, &main.join(".claude/worktrees/x"), &[]);
    h.t.write(&format!("{}/uv.lock", h.rel(&wt)), "");
    h.t.write(&format!("{}/.venv/pyvenv.cfg", h.rel(&wt)), "");
    git::worktree(&main, &h.t.path("orphan"), &[]);
    fs::write(h.t.path("orphan/.git"), format!("gitdir: {}\n", h.t.path("gone/.git/worktrees/o").display())).unwrap();

    let r = h.scan();

    for rule in ["git.worktree", "git.worktree-orphan", "python.venv"] {
        assert!(r.findings.iter().any(|f| f.rule == rule), "{rule}: {:?}", h.summary(&r));
    }
    let value = serde_json::to_value(&r).unwrap();
    let doc: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/scan.schema.json")).unwrap(),
    )
    .unwrap();
    let validator = jsonschema::validator_for(&doc).unwrap();
    let errors: Vec<String> = validator.iter_errors(&value).map(|e| format!("{e} at {}", e.instance_path())).collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let back: ScanReport = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(back, r, "read back unchanged");
    let wt_json = &value["findings"].as_array().unwrap().iter().find(|f| f["rule"] == "git.worktree").unwrap();
    assert_eq!(wt_json["origin"], serde_json::json!({"harness": "claude-code", "kind": "worktree"}));
    assert_eq!(wt_json["worktree"]["probe"], "unknown");
    assert!(wt_json["worktree"].get("clean").is_none(), "a field no probe filled is absent");
    let venv_json = value["findings"].as_array().unwrap().iter().find(|f| f["rule"] == "python.venv").unwrap();
    assert_eq!(venv_json["project"]["worktree_of"], main.to_str().unwrap());
}
