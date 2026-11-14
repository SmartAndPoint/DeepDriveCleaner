//! `ddc agent clean` end to end (story "Clean: --agent bounded consent",
//! epic DDC-003): a temporary home with a real repository, a dead and a
//! dirty Claude Code worktree, a second project and a package cache, made
//! with the `git` on `PATH`. Only `safe` project artifacts inside the
//! project's checkouts go, and only with the opt-in.

#![cfg(unix)]

use ddc_core::platform::Canonical as _;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime};

use ddc_core::agents::AgentsReport;
use ddc_core::env::Env;
use ddc_core::exec::report::CleanReport;
use ddc_core::exec::{Outcome, SkipReason};
use ddc_core::holders::HolderCommand;
use ddc_core::plan::ListedItems;
use ddc_core::store::{self, LockMode};
use ddc_loadgen::isolate::ClearEnv as _;

mod common;
mod contract;

/// Older than the projects' window (14 days) and the worktrees' (2).
const OLD: Duration = Duration::from_secs(30 * 86_400);

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    data: PathBuf,
    main: PathBuf,
    cache: PathBuf,
    tool_cache: PathBuf,
    venv: PathBuf,
    dead: PathBuf,
    dead_cache: PathBuf,
    dirty: PathBuf,
    other_cache: PathBuf,
    npm: PathBuf,
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// Every file under `dir` last modified [`OLD`] ago: what tells a project's
/// and a worktree's activity (git's index and HEAD, the lock files).
fn age(dir: &Path) {
    let then = SystemTime::now() - OLD;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in fs::read_dir(&d).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                // Its owner may set the times of a read-only file (git's objects).
                fs::File::open(entry.path()).unwrap().set_modified(then).unwrap();
            }
        }
    }
}

impl Fx {
    /// `proj` in the home, named `name`: `pkg/__pycache__` and
    /// `.pytest_cache` in its main checkout (`safe`), an idle `.venv` with
    /// `uv.lock` (`rebuildable`), a dead Claude Code worktree holding a
    /// `__pycache__`, and a dirty one; beside it `other` with a
    /// `__pycache__`, and the npm cache. `None` without a git.
    fn ready_named(test: &str, name: &str) -> Option<Self> {
        if common::unsupported(test) {
            return None;
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonical().unwrap();
        let (home, data) = (root.join("home"), root.join("data"));
        let main = home.join(name);
        fs::create_dir_all(&main).unwrap();
        fs::create_dir_all(&data).unwrap();
        common::skipped_without(test, common::git(&main, &["init", "-q", "-b", "main"]))?;
        write(&main.join(".gitignore"), ".venv/\n__pycache__/\n.pytest_cache/\n");
        write(&main.join("uv.lock"), "version = 1\n");
        common::git(&main, &["add", "."]).unwrap();
        common::git(&main, &["commit", "-q", "-m", "init"]).unwrap();
        let worktree = |name: &str| {
            let path = main.join(".claude/worktrees").join(name);
            common::git(&main, &["worktree", "add", "-q", "-b", &format!("worktree-{name}"), path.to_str().unwrap()])
                .unwrap();
            path
        };
        let (dead, dirty) = (worktree("dead"), worktree("dirty"));
        let pyc = "pkg/__pycache__/m.cpython-312.pyc";
        write(&main.join(pyc), &"p".repeat(20_000));
        write(&main.join(".pytest_cache/v/cache/nodeids"), &"t".repeat(5_000));
        write(&main.join(".venv/pyvenv.cfg"), "home = /usr/bin\n");
        write(&main.join(".venv/lib/site.py"), &"x".repeat(50_000));
        write(&dead.join(pyc), &"q".repeat(10_000));
        write(&dirty.join("new.txt"), "work in progress\n");
        write(&home.join("other").join(pyc), &"o".repeat(10_000));
        write(&home.join(".npm/_cacache/index-v5/00/x"), &"n".repeat(10_000));
        age(&root);
        Some(Self {
            cache: main.join("pkg/__pycache__"),
            tool_cache: main.join(".pytest_cache"),
            venv: main.join(".venv"),
            dead_cache: dead.join("pkg/__pycache__"),
            other_cache: home.join("other/pkg/__pycache__"),
            npm: home.join(".npm/_cacache"),
            _tmp: tmp,
            root,
            home,
            data,
            main,
            dead,
            dirty,
        })
    }

    fn ready(test: &str) -> Option<Self> {
        Self::ready_named(test, "proj")
    }

    fn settings(&self, json: &str) {
        write(&self.data.join("settings.json"), json);
    }

    fn opt_in(&self) {
        self.settings(r#"{"schema_version": 1, "agents_clean_safe": true}"#);
    }

    fn journal(&self) -> PathBuf {
        self.data.join("logs/actions.jsonl")
    }

    #[allow(clippy::disallowed_methods)] // the test's own PATH, for the git the fixture used
    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"));
        cmd.clear_env()
            .home_env(&self.home)
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("NO_COLOR", "1")
            .args(args)
            .arg("--data-dir")
            .arg(&self.data)
            .stdin(Stdio::null());
        cmd
    }

    /// `ddc agent clean ARGS --data-dir DATA`.
    fn agent_clean(&self, args: &[&str]) -> Output {
        let mut all = vec!["agent", "clean"];
        all.extend_from_slice(args);
        self.command(&all).output().unwrap()
    }

    fn report(&self, args: &[&str]) -> CleanReport {
        let out = self.agent_clean(args);
        assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
        contract::holds("clean", &out.stdout)
    }

    /// `ddc clean --items -` fed `list` on stdin.
    fn clean_items(&self, list: &[u8]) -> Output {
        let mut child = self
            .command(&["clean", "--items", "-", "--json"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(list).unwrap();
        child.wait_with_output().unwrap()
    }

    fn main_str(&self) -> &str {
        self.main.to_str().unwrap()
    }
}

fn outcome<'a>(r: &'a CleanReport, path: &Path) -> &'a Outcome {
    &r.items.iter().find(|i| i.path == path).unwrap_or_else(|| panic!("{path:?} not in {:#?}", r.items)).outcome
}

fn for_person(r: &CleanReport) -> Vec<PathBuf> {
    r.for_person.as_ref().map(|l| l.items.iter().map(|i| i.path.clone()).collect()).unwrap_or_default()
}

fn considered(r: &CleanReport, path: &Path) -> bool {
    r.items.iter().any(|i| i.path.starts_with(path))
}

#[test]
fn without_the_opt_in_it_removes_nothing_and_says_why_for_each_item() {
    let Some(fx) = Fx::ready("without_the_opt_in_it_removes_nothing_and_says_why_for_each_item") else { return };

    let r = fx.report(&[fx.main_str(), "--json"]);

    assert_eq!(r.agents_clean_safe, Some(false));
    assert_eq!(r.project.as_deref(), Some(fx.main.as_path()));
    for path in [&fx.cache, &fx.tool_cache, &fx.dead_cache] {
        assert_eq!(outcome(&r, path), &Outcome::Skipped(SkipReason::AgentOptInOff), "{path:?}");
        assert!(path.exists());
    }
    for path in [&fx.venv, &fx.dead, &fx.dirty] {
        assert_eq!(outcome(&r, path), &Outcome::Skipped(SkipReason::NotConsented), "{path:?}");
    }
    assert!(!considered(&r, &fx.other_cache) && !considered(&r, &fx.npm), "another project, a shared cache");
    assert!(!fx.journal().exists(), "no journal line");
    assert!(!fx.data.join(store::LAST_SCAN).exists(), "no scan report");
    assert_eq!(
        for_person(&r),
        [fx.dead.clone(), fx.tool_cache.clone(), fx.venv.clone(), fx.cache.clone()],
        "the dead worktree, the .venv and the safe items it held back; nothing inside the worktree"
    );
}

#[test]
fn with_the_opt_in_it_removes_the_safe_items_inside_the_checkouts_only() {
    let Some(fx) = Fx::ready("with_the_opt_in_it_removes_the_safe_items_inside_the_checkouts_only") else { return };
    fx.opt_in();

    let r = fx.report(&[fx.main_str(), "--json"]);

    assert_eq!(r.agents_clean_safe, Some(true));
    for path in [&fx.cache, &fx.tool_cache, &fx.dead_cache] {
        assert_eq!(outcome(&r, path), &Outcome::Deleted, "{path:?}");
        assert!(!path.exists(), "{path:?}");
    }
    for path in [&fx.dead, &fx.dirty, &fx.venv, &fx.other_cache, &fx.npm] {
        assert!(path.exists(), "{path:?}");
    }
    let journal = fs::read_to_string(fx.journal()).unwrap();
    let lines: Vec<serde_json::Value> = journal.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(lines.len(), 3, "{journal}");
    for line in &lines {
        assert_eq!(line["consent"], "agent", "{line}");
        assert_eq!(line["project"], fx.main_str(), "{line}");
    }
    assert!(!fx.data.join(store::LAST_SCAN).exists(), "no scan report");
    assert_eq!(for_person(&r), [fx.dead.clone(), fx.venv.clone()], "the dead worktree and the .venv");

    // The person's list, as `ddc clean --items` reads it: the worktree goes through git.
    let list = serde_json::to_vec(r.for_person.as_ref().unwrap()).unwrap();
    let out = fx.clean_items(&list);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let done: CleanReport = contract::holds("clean", &out.stdout);
    assert!(matches!(outcome(&done, &fx.dead), Outcome::Native { .. }), "{:?}", outcome(&done, &fx.dead));
    assert!(!fx.dead.exists() && !fx.venv.exists());
    let listed = common::git(&fx.main, &["worktree", "list"]).unwrap();
    assert!(!listed.contains("worktrees/dead"), "{listed}");
}

#[test]
fn the_text_prints_the_persons_command_with_a_quoted_heredoc() {
    let Some(fx) = Fx::ready("the_text_prints_the_persons_command_with_a_quoted_heredoc") else { return };

    let out = fx.agent_clean(&[fx.main_str()]);

    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("ddc clean --items - <<'DDC_ITEMS'\n"), "{text}");
    assert!(text.contains("once the person allows it"), "says who lets agents clean: {text}");
    let r = fx.report(&[fx.main_str(), "--json"]);
    for path in for_person(&r) {
        assert!(text.contains(&format!("{{\"path\":\"{}\"", path.display())), "{path:?} on a line of its own: {text}");
    }
}

/// The command a person pastes, run by `sh` against a stub `ddc` that
/// records its arguments and stdin.
fn run_pasted(fx: &Fx, text: &str) -> (Vec<String>, Vec<u8>) {
    let start = text.find("\nddc clean ").unwrap_or_else(|| panic!("no command: {text}")) + 1;
    let end = text[start..].find("\nDDC_ITEMS").unwrap() + start + "\nDDC_ITEMS".len();
    let command = &text[start..end];
    let bin = fx.root.join("bin");
    let (args, stdin) = (fx.root.join("args"), fx.root.join("stdin"));
    write(
        &bin.join("ddc"),
        &format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\ncat > '{}'\n", args.display(), stdin.display()),
    );
    common::set_mode(bin.join("ddc"), 0o755);
    let out = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("{command}\n"))
        .clear_env()
        .env("HOME", &fx.home)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let args = fs::read_to_string(args).unwrap().lines().map(str::to_owned).collect();
    (args, fs::read(stdin).unwrap())
}

#[test]
fn a_pasted_command_hands_ddc_the_list_exactly() {
    let name = "p $HOME `id` 'q' \"d\"";
    let Some(fx) = Fx::ready_named("a_pasted_command_hands_ddc_the_list_exactly", name) else { return };
    let r = fx.report(&[fx.main_str(), "--json"]);
    let text = String::from_utf8_lossy(&fx.agent_clean(&[fx.main_str()]).stdout).into_owned();

    let (args, stdin) = run_pasted(&fx, &text);

    assert_eq!(args, ["clean", "--items", "-"]);
    let read = ListedItems::parse(&stdin).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&stdin)));
    assert_eq!(Some(&read), r.for_person.as_ref(), "the shell expanded nothing");
}

#[test]
fn outside_the_scan_roots_the_command_names_the_checkouts_parents() {
    let Some(fx) = Fx::ready("outside_the_scan_roots_the_command_names_the_checkouts_parents") else { return };
    let elsewhere = fx.home.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fx.settings(&format!(r#"{{"schema_version": 1, "roots": [{}]}}"#, serde_json::to_string(&elsewhere).unwrap()));

    let text = String::from_utf8_lossy(&fx.agent_clean(&[fx.main_str()]).stdout).into_owned();

    let (args, _) = run_pasted(&fx, &text);
    let home = fx.home.display().to_string();
    let worktrees = fx.main.join(".claude/worktrees").display().to_string();
    assert_eq!(args, ["clean", home.as_str(), worktrees.as_str(), "--items", "-"], "{text}");
}

#[test]
fn a_long_list_or_a_name_a_terminal_cannot_show_prints_no_command() {
    let Some(fx) = Fx::ready("a_long_list_or_a_name_a_terminal_cannot_show_prints_no_command") else { return };
    for i in 0..50 {
        write(&fx.main.join(format!("m{i}/__pycache__/x.pyc")), "c");
    }

    let text = String::from_utf8_lossy(&fx.agent_clean(&[fx.main_str()]).stdout).into_owned();

    assert!(!text.contains("DDC_ITEMS"), "{text}");
    assert!(text.contains("ddc clean --items FILE"), "{text}");
    // Moved out of the project, not removed: a test removes nothing itself.
    let gone = fx.root.join("gone");
    fs::create_dir_all(&gone).unwrap();
    for i in 0..50 {
        fs::rename(fx.main.join(format!("m{i}")), gone.join(format!("m{i}"))).unwrap();
    }
    write(&fx.main.join("tab\there/__pycache__/x.pyc"), "c");
    let text = String::from_utf8_lossy(&fx.agent_clean(&[fx.main_str()]).stdout).into_owned();
    assert!(!text.contains("DDC_ITEMS"), "a control character: {text}");
    fs::rename(fx.main.join("tab\there"), gone.join("tab")).unwrap();
    let text = String::from_utf8_lossy(&fx.agent_clean(&[fx.main_str()]).stdout).into_owned();
    assert!(text.contains("DDC_ITEMS"), "the command is back: {text}");

    // A name that is not UTF-8, where the file system takes one (APFS does not).
    if ddc_core::platform::PLATFORM == ddc_core::platform::Platform::Linux {
        use std::os::unix::ffi::OsStrExt as _;
        let odd = fx.main.join(std::ffi::OsStr::from_bytes(b"bad-\xff"));
        write(&odd.join("__pycache__/x.pyc"), "c");
        let text = String::from_utf8_lossy(&fx.agent_clean(&[fx.main_str()]).stdout).into_owned();
        assert!(!text.contains("DDC_ITEMS"), "not UTF-8: {text}");
    }
}

#[test]
fn a_worktree_resolves_to_its_project_and_a_folder_outside_one_is_refused() {
    let Some(fx) = Fx::ready("a_worktree_resolves_to_its_project_and_a_folder_outside_one_is_refused") else { return };

    let r = fx.report(&[fx.dead.to_str().unwrap(), "--json"]);
    assert_eq!(r.project.as_deref(), Some(fx.main.as_path()));
    assert!(considered(&r, &fx.dirty) && considered(&r, &fx.cache), "the sibling worktree and the main checkout");

    let plain = fx.home.join("plain");
    fs::create_dir_all(&plain).unwrap();
    let out = fx.agent_clean(&[plain.to_str().unwrap(), "--json"]);
    assert_eq!(out.status.code(), Some(2));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["error"]["code"], "not-a-repository");

    common::git(&fx.home, &["init", "-q", "-b", "main"]).unwrap();
    let out = fx.agent_clean(&[plain.to_str().unwrap(), "--json"]);
    assert_eq!(out.status.code(), Some(2));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["error"]["code"], "home-is-the-checkout");
}

#[test]
fn a_list_narrows_the_clean_and_never_widens_it() {
    let Some(fx) = Fx::ready("a_list_narrows_the_clean_and_never_widens_it") else { return };
    // A review item inside the checkout: a `node_modules` without its manifest.
    let orphan = fx.main.join("old/node_modules");
    write(&orphan.join("dep/index.js"), &"j".repeat(5_000));
    age(&fx.main.join("old"));
    fx.opt_in();
    let seen = fx.report(&[fx.main_str(), "--json", "--dry-run"]);
    let item = |path: &Path| {
        let mut v = serde_json::to_value(seen.items.iter().find(|i| i.path == path).unwrap()).unwrap();
        let v = v.as_object_mut().unwrap();
        v.retain(|k, _| ["path", "dev", "ino", "rule", "class", "action"].contains(&k.as_str()));
        serde_json::Value::Object(v.clone())
    };
    let made = |path: &Path, rule: &str, class: &str| {
        let (dev, ino) = common::id(path);
        serde_json::json!({"path": path, "dev": dev, "ino": ino, "rule": rule, "class": class, "action": "delete"})
    };
    // Listed for deletion for good: valid in a person's list, never an agent's.
    let mut review = item(&orphan);
    assert_eq!(review["class"], "review", "{review}");
    assert_eq!(review["rule"], "node.node_modules-orphan", "{review}");
    review["action"] = "delete".into();
    review["permanent"] = true.into();
    let list = serde_json::json!({"schema_version": 1, "items": [
        item(&fx.cache), item(&fx.venv), review,
        made(&fx.other_cache, "python.pycache", "safe"), made(&fx.npm, "cache.npm", "safe"),
    ]});
    let file = fx.root.join("list.json");
    fs::write(&file, serde_json::to_vec(&list).unwrap()).unwrap();

    let r = fx.report(&[fx.main_str(), "--json", "--items", file.to_str().unwrap()]);

    let outcomes: Vec<(&Path, &Outcome)> = r.items.iter().map(|i| (i.path.as_path(), &i.outcome)).collect();
    let refused = Outcome::Skipped(SkipReason::NotConsented);
    assert_eq!(
        outcomes,
        [
            (fx.cache.as_path(), &Outcome::Deleted),
            (fx.venv.as_path(), &refused),
            (orphan.as_path(), &refused),
            (fx.other_cache.as_path(), &refused),
            (fx.npm.as_path(), &refused),
        ],
        "in the list's order"
    );
    assert!(!fx.cache.exists() && fx.tool_cache.exists() && fx.venv.exists() && fx.other_cache.exists());
    assert!(orphan.exists() && fx.npm.exists());

    // A malformed list: refused before the tree or the journal is touched.
    let journal = fs::read(fx.journal()).unwrap();
    fs::write(&file, b"{\"schema_version\": 1, \"items\": [{}]}").unwrap();
    let out = fx.agent_clean(&[fx.main_str(), "--json", "--items", file.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(fx.tool_cache.exists());
    assert_eq!(fs::read(fx.journal()).unwrap(), journal);
}

#[test]
fn a_dry_run_takes_no_lock_and_a_real_run_waits_for_none() {
    let Some(fx) = Fx::ready("a_dry_run_takes_no_lock_and_a_real_run_waits_for_none") else { return };
    fx.opt_in();
    let env = Env::with_paths(&fx.home, &fx.data, &fx.data.join("logs"), SystemTime::now()).unwrap();
    let held = store::try_lock(&env, LockMode::Exclusive, HolderCommand::Clean).unwrap();

    let r = fx.report(&[fx.main_str(), "--json", "--dry-run"]);
    assert!(r.dry_run);
    assert_eq!(outcome(&r, &fx.cache), &Outcome::NotRun);
    assert!(fx.cache.exists() && !fx.journal().exists());

    let out = fx.agent_clean(&[fx.main_str(), "--json"]);
    assert_eq!(out.status.code(), Some(75));
    let busy: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(busy["error"]["code"], "busy");
    assert!(fx.cache.exists() && !fx.journal().exists());
    drop(held);

    // While one runs, the lock names it.
    let _agent = store::try_lock(&env, LockMode::Exclusive, HolderCommand::AgentClean).unwrap();
    let out = fx.command(&["lock", "status", "--json"]).output().unwrap();
    let status: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status["holders"][0]["command"], "agent-clean", "{status}");
}

#[test]
fn unreadable_settings_consider_nothing_and_name_the_newer_key() {
    let Some(fx) = Fx::ready("unreadable_settings_consider_nothing_and_name_the_newer_key") else { return };
    fx.settings("{ not json");

    let r = fx.report(&[fx.main_str(), "--json"]);
    assert!(r.items.is_empty() && r.for_person.is_none());
    assert_eq!(r.agents_clean_safe, Some(false));
    assert!(r.settings_error.is_some());
    assert!(fx.cache.exists() && !fx.journal().exists());

    // A key from a newer ddc: the error names it and says to update.
    fx.settings(r#"{"schema_version": 1, "agents_clean_safe": true, "agents_clean_review": true}"#);
    let r = fx.report(&[fx.main_str(), "--json"]);
    let error = r.settings_error.unwrap();
    assert!(error.contains("agents_clean_review") && error.contains("update ddc"), "{error}");
    assert!(fx.cache.exists());
}

#[test]
fn agent_status_says_whether_agents_may_clean() {
    let Some(fx) = Fx::ready("agent_status_says_whether_agents_may_clean") else { return };
    let sys = fx.root.join("sys");
    fs::create_dir_all(&sys).unwrap();
    let status = |fx: &Fx| -> AgentsReport {
        let out = fx
            .command(&["agent", "status", "--json", "--no-sizes", "--system-root", sys.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        contract::holds("agents", &out.stdout)
    };
    assert!(!status(&fx).agents_clean_safe);
    fx.opt_in();
    assert!(status(&fx).agents_clean_safe);
}
