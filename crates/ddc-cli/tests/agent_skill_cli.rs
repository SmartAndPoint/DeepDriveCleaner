//! `ddc agent skill install` and `ddc agent skill status` end to end (spec
//! "Agent skill install", alias `skill-install`; story "Agent guidance: skill
//! and AGENTS.md snippet", epic DDC-003), on a synthetic home.

use ddc_core::platform::Canonical as _;
use ddc_loadgen::isolate::ClearEnv as _;
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use ddc_core::guidance::{self, SKILL_FILES, SNIPPET};

mod common;

struct Fx {
    _tmp: tempfile::TempDir,
    home: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonical().unwrap();
        let fx = Self { home: root.join("home"), _tmp: tmp };
        fs::create_dir_all(fx.home.join("p")).unwrap();
        fx
    }

    fn skill(&self) -> PathBuf {
        guidance::skill_folder(&self.home)
    }

    fn shared(&self) -> PathBuf {
        guidance::Family::Agents.folder(&self.home)
    }

    fn ddc(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"));
        cmd.clear_env().home_env(&self.home).env("PATH", "").env("NO_COLOR", "1").args(args).stdin(Stdio::null());
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.ddc(args).output().unwrap()
    }

    /// `ddc` under a pseudo-terminal, answering `answer`.
    fn in_terminal(&self, test: &str, args: &[&str], answer: &str) -> Option<(Option<i32>, String)> {
        let ddc = self.ddc(args);
        let mut script =
            common::skipped_without(test, common::script(ddc.get_program(), &ddc.get_args().collect::<Vec<_>>()))?;
        script.clear_env();
        for (k, v) in ddc.get_envs() {
            script.env(k, v.unwrap());
        }
        let mut child = script.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(answer.as_bytes()).unwrap();
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            assert!(started.elapsed() < Duration::from_secs(60), "ddc did not finish");
            std::thread::sleep(Duration::from_millis(20));
        };
        drop(stdin);
        let mut out = String::new();
        child.stdout.take().unwrap().read_to_string(&mut out).unwrap();
        Some((status.code(), out))
    }
}

fn text(out: &Output) -> (String, String) {
    (String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

// [skill-install#8]: no terminal and no --approved writes nothing; with it,
// the home's skill is written; again, nothing changes.
#[test]
fn without_a_terminal_it_needs_approved_and_then_writes_once() {
    if common::unsupported("without_a_terminal_it_needs_approved_and_then_writes_once") {
        return;
    }
    let fx = Fx::new();
    let out = fx.run(&["agent", "skill", "install"]);
    let (stdout, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("--approved once the person has agreed"), "{stderr}");
    assert!(
        stdout.contains(&fx.skill().join("SKILL.md").display().to_string()),
        "it lists what it would write: {stdout}"
    );
    assert!(!fx.home.join(".claude").exists() && !fx.home.join(".agents").exists(), "nothing written");

    let out = fx.run(&["agent", "skill", "install", "--approved"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    // [skill-install#1]: both families by default.
    for dir in [fx.skill(), fx.shared()] {
        assert_eq!(fs::read_to_string(dir.join("SKILL.md")).unwrap(), SKILL_FILES[0].1, "{}", dir.display());
        assert!(dir.join(guidance::MARKER).is_file(), "{}", dir.display());
    }

    let before = fs::metadata(fx.skill().join("SKILL.md")).unwrap().modified().unwrap();
    let out = fx.run(&["agent", "skill", "install"]);
    let (stdout, _) = text(&out);
    assert_eq!(out.status.code(), Some(0), "current: no question, no --approved needed");
    assert!(stdout.contains("current"), "{stdout}");
    assert_eq!(fs::metadata(fx.skill().join("SKILL.md")).unwrap().modified().unwrap(), before);
}

// [skill-install#8]: at a terminal it lists the files and asks; a No writes nothing.
#[test]
fn at_a_terminal_it_lists_the_files_and_asks() {
    if common::unsupported("at_a_terminal_it_lists_the_files_and_asks") {
        return;
    }
    let fx = Fx::new();
    let Some((code, no)) =
        fx.in_terminal("at_a_terminal_it_lists_the_files_and_asks", &["agent", "skill", "install"], "n\n")
    else {
        return;
    };
    assert!(no.contains("delete nothing") && no.contains("SKILL.md") && no.contains("[y/N]"), "{no}");
    assert!(no.contains("Nothing changed."), "{no}");
    assert_eq!(code, Some(1), "a No exits 1");
    assert!(!fx.home.join(".claude").exists());

    let Some((code, yes)) =
        fx.in_terminal("at_a_terminal_it_lists_the_files_and_asks", &["agent", "skill", "install"], "y\n")
    else {
        return;
    };
    assert_eq!(code, Some(0), "{yes}");
    assert!(fx.skill().join("SKILL.md").is_file());
}

// [skill-install#1] [skill-install#7]: a project's skill and its AGENTS.md, nowhere else.
#[test]
fn a_project_gets_its_skill_and_its_agents_md_block() {
    if common::unsupported("a_project_gets_its_skill_and_its_agents_md_block") {
        return;
    }
    let fx = Fx::new();
    let project = fx.home.join("p");
    let agents_md = project.join("AGENTS.md");
    fs::write(&agents_md, "# Project rules\n").unwrap();
    let out = fx.run(&[
        "agent",
        "skill",
        "install",
        "--approved",
        "--project",
        project.to_str().unwrap(),
        "--agents-md",
        agents_md.to_str().unwrap(),
    ]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    assert!(guidance::skill_folder(&project).join("SKILL.md").is_file());
    assert!(guidance::Family::Agents.folder(&project).join("SKILL.md").is_file());
    assert!(!fx.home.join(".claude").exists() && !fx.home.join(".agents").exists(), "not the home's");
    assert_eq!(fs::read_to_string(&agents_md).unwrap(), format!("# Project rules\n\n{SNIPPET}"));

    let missing = fx.home.join("nowhere");
    let out = fx.run(&["agent", "skill", "install", "--approved", "--project", missing.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out).1.contains("no such folder"), "{}", text(&out).1);
    let out = fx.run(&["agent", "skill", "status", "--project", missing.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2), "status refuses a missing folder too");
    let out = fx.run(&["agent", "skill", "install", "--approved", "--project", agents_md.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out).1.contains("not a folder"), "{}", text(&out).1);
    let nowhere = missing.join("AGENTS.md");
    let nowhere = nowhere.to_str().unwrap();
    for args in [
        &["agent", "skill", "install", "--approved", "--agents-md", nowhere][..],
        &["agent", "skill", "status", "--agents-md", nowhere],
    ] {
        let out = fx.run(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}: {}", text(&out).1);
        assert!(text(&out).1.contains("its folder does not exist"), "{args:?}: {}", text(&out).1);
    }
    assert!(!fx.home.join(".claude").exists(), "a refused install writes the home's skill neither");
}

// [skill-install#4]: an AGENTS.md that is a link is refused, and nothing is written.
#[cfg(unix)]
#[test]
fn a_linked_agents_md_is_refused_and_nothing_is_written() {
    if common::unsupported("a_linked_agents_md_is_refused_and_nothing_is_written") {
        return;
    }
    let fx = Fx::new();
    let target = fx.home.join("p/notes.md");
    fs::write(&target, "mine\n").unwrap();
    let link = fx.home.join("p/AGENTS.md");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let out = fx.run(&["agent", "skill", "install", "--approved", "--agents-md", link.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out).1);
    assert!(text(&out).1.contains("symbolic link"), "{}", text(&out).1);
    assert_eq!(fs::read_to_string(&target).unwrap(), "mine\n");
    assert!(!fx.home.join(".claude").exists(), "nothing written");
}

// [skill-install#3]: an edited skill is refused until --replace.
#[test]
fn an_edited_skill_is_refused_until_replace() {
    if common::unsupported("an_edited_skill_is_refused_until_replace") {
        return;
    }
    let fx = Fx::new();
    assert_eq!(fx.run(&["agent", "skill", "install", "--approved"]).status.code(), Some(0));
    fs::write(fx.skill().join("SKILL.md"), "my own notes\n").unwrap();
    let out = fx.run(&["agent", "skill", "install", "--approved"]);
    let (_, stderr) = text(&out);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("edited") && stderr.contains("--replace"), "{stderr}");
    assert_eq!(fs::read_to_string(fx.skill().join("SKILL.md")).unwrap(), "my own notes\n");
    assert_eq!(fx.run(&["agent", "skill", "install", "--approved", "--replace"]).status.code(), Some(0));
    assert_eq!(fs::read_to_string(fx.skill().join("SKILL.md")).unwrap(), SKILL_FILES[0].1);
}

// [skill-install#9] [skill-install#10]: status reads and reports.
#[test]
fn status_reports_each_place_and_writes_nothing() {
    if common::unsupported("status_reports_each_place_and_writes_nothing") {
        return;
    }
    let fx = Fx::new();
    let agents_md = fx.home.join("p/AGENTS.md");
    fs::write(&agents_md, "<!-- ddc:begin -->\nolder\n<!-- ddc:end -->\n").unwrap();
    let json = |out: &Output| -> serde_json::Value { serde_json::from_slice(&out.stdout).unwrap() };
    let out = fx.run(&["agent", "skill", "status", "--json", "--agents-md", agents_md.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(0));
    let v = json(&out);
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["skills"][0]["state"], "absent");
    assert_eq!(v["agents_md"]["state"], "differs");
    assert!(!fx.home.join(".claude").exists(), "status writes nothing");

    assert_eq!(fx.run(&["agent", "skill", "install", "--approved"]).status.code(), Some(0));
    let v = json(&fx.run(&["agent", "skill", "status", "--json"]));
    assert_eq!((&v["skills"][0]["family"], &v["skills"][0]["state"]), (&"claude-code".into(), &"current".into()));
    assert_eq!((&v["skills"][1]["family"], &v["skills"][1]["state"]), (&"agents".into(), &"current".into()));
    assert_eq!(v["ddc_version"], env!("CARGO_PKG_VERSION"));
    let (stdout, _) = text(&fx.run(&["agent", "skill", "status"]));
    assert!(stdout.contains("current"), "{stdout}");
}

// [skill-install#1]: --family narrows it; a refusal in one folder writes neither.
#[test]
fn family_narrows_the_install_and_one_refusal_stops_both() {
    if common::unsupported("family_narrows_the_install_and_one_refusal_stops_both") {
        return;
    }
    let fx = Fx::new();
    let out = fx.run(&["agent", "skill", "install", "--approved", "--family", "agents"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    assert!(fx.shared().join("SKILL.md").is_file());
    assert!(!fx.home.join(".claude").exists(), "only the shared folder");

    fs::write(fx.shared().join("SKILL.md"), "edited by hand\n").unwrap();
    let out = fx.run(&["agent", "skill", "install", "--approved"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out).1);
    assert!(text(&out).1.contains("edited"), "{}", text(&out).1);
    assert!(!fx.home.join(".claude").exists(), "Claude Code's folder is not written either");
    assert_eq!(fs::read_to_string(fx.shared().join("SKILL.md")).unwrap(), "edited by hand\n");
    let out = fx.run(&["agent", "skill", "install", "--approved", "--family", "claude-code"]);
    assert_eq!(out.status.code(), Some(0), "the edited shared folder is not asked about: {}", text(&out).1);
    assert!(fx.skill().join("SKILL.md").is_file());
}

// [skill-install#4] [skill-install#1]: a linked shared folder refuses the whole install.
#[cfg(unix)]
#[test]
fn a_linked_shared_folder_refuses_the_whole_install() {
    if common::unsupported("a_linked_shared_folder_refuses_the_whole_install") {
        return;
    }
    let fx = Fx::new();
    let elsewhere = fx.home.join("p/elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    fs::create_dir_all(fx.shared().parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&elsewhere, fx.shared()).unwrap();
    let out = fx.run(&["agent", "skill", "install", "--approved"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out).1);
    assert!(text(&out).1.contains("symbolic link"), "{}", text(&out).1);
    assert!(!fx.home.join(".claude").exists(), "Claude Code's folder is not written either");
    assert!(fs::read_dir(&elsewhere).unwrap().next().is_none(), "nothing written through the link");
}

// [skill-install#9]: with --project, four folders: the home's and the project's, each family.
#[test]
fn status_with_a_project_reports_both_families_of_both_places() {
    if common::unsupported("status_with_a_project_reports_both_families_of_both_places") {
        return;
    }
    let fx = Fx::new();
    let project = fx.home.join("p");
    let p = project.to_str().unwrap();
    assert_eq!(
        fx.run(&["agent", "skill", "install", "--approved", "--project", p, "--family", "agents"]).status.code(),
        Some(0)
    );
    let out = fx.run(&["agent", "skill", "status", "--json", "--project", p]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out).1);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let rows: Vec<(String, String, String)> = v["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["path"].as_str().unwrap().to_owned(),
                s["family"].as_str().unwrap().to_owned(),
                s["state"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let path = |dir: PathBuf| dir.display().to_string();
    assert_eq!(
        rows,
        [
            (path(fx.skill()), "claude-code".to_owned(), "absent".to_owned()),
            (path(fx.shared()), "agents".to_owned(), "absent".to_owned()),
            (path(guidance::skill_folder(&project)), "claude-code".to_owned(), "absent".to_owned()),
            (path(guidance::Family::Agents.folder(&project)), "agents".to_owned(), "current".to_owned()),
        ]
    );
}
