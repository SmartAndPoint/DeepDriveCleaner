//! The guidance `ddc agent skill install` writes (spec "Agent skill install",
//! alias `skill-install`): the skill folder, its marker and the `AGENTS.md`
//! block, on temporary folders.

use std::fs;
use std::path::Path;

use ddc_core::guidance::{
    self, BEGIN, END, FileState, FolderRefusal, FolderState, MARKER, Marker, PlanError, SKILL_FILES, SNIPPET,
};

const V: &str = "9.9.9";

fn skill(dir: &Path) -> String {
    fs::read_to_string(dir.join("SKILL.md")).unwrap()
}

fn install(dir: &Path, replace: bool) -> Result<Vec<String>, PlanError> {
    let plan = guidance::folder_plan(dir, V, replace)?;
    guidance::write_folder(&plan).unwrap();
    Ok(plan.writes.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect())
}

/// The copies built into the binary are the files in `agents/`, byte for byte.
#[test]
fn the_built_in_copies_are_the_files_in_agents() {
    let agents = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../agents");
    for (name, text) in SKILL_FILES {
        assert_eq!(fs::read_to_string(agents.join("claude-code/ddc").join(name)).unwrap(), *text, "{name}");
    }
    assert_eq!(fs::read_to_string(agents.join("AGENTS.snippet.md")).unwrap(), SNIPPET);
    assert!(SNIPPET.starts_with(&format!("{BEGIN}\n")) && SNIPPET.ends_with(&format!("{END}\n")));
}

// [skill-install#2] [skill-install#5]
#[test]
fn an_absent_folder_gets_the_files_then_the_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = guidance::skill_folder(tmp.path());
    assert_eq!(guidance::folder_status(&dir).unwrap().state, FolderState::Absent);
    assert_eq!(install(&dir, false).unwrap(), ["SKILL.md", MARKER]);
    assert_eq!(skill(&dir), SKILL_FILES[0].1);
    let marker: Marker = serde_json::from_slice(&fs::read(dir.join(MARKER)).unwrap()).unwrap();
    assert_eq!(marker, Marker::of_this_copy(V));
    assert_eq!(marker.files["SKILL.md"].len(), 16, "a 64-bit hash in hexadecimal");
    assert_eq!(guidance::folder_status(&dir).unwrap().state, FolderState::Current);
}

#[test]
fn fnv1a64_is_the_published_function() {
    assert_eq!(guidance::fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(guidance::fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
}

// [skill-install#8]: this copy in place writes nothing.
#[test]
fn a_current_folder_plans_no_write() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = guidance::skill_folder(tmp.path());
    install(&dir, false).unwrap();
    let modified = |name: &str| fs::metadata(dir.join(name)).unwrap().modified().unwrap();
    let before = (modified("SKILL.md"), modified(MARKER));
    std::thread::sleep(std::time::Duration::from_millis(20));
    for replace in [false, true] {
        let plan = guidance::folder_plan(&dir, V, replace).unwrap();
        assert!(plan.writes.is_empty(), "replace {replace}");
        guidance::write_folder(&plan).unwrap();
    }
    assert_eq!((modified("SKILL.md"), modified(MARKER)), before);
}

// [skill-install#2]: an older copy is replaced; other files stay and are named.
#[test]
fn an_older_copy_is_replaced_and_other_files_stay() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = guidance::skill_folder(tmp.path());
    fs::create_dir_all(&dir).unwrap();
    let old = "---\nname: ddc\nmetadata:\n  version: \"0.1.0\"\n---\nold\n";
    fs::write(dir.join("SKILL.md"), old).unwrap();
    fs::write(dir.join("helper.md"), "old helper\n").unwrap();
    fs::write(dir.join("notes.md"), "mine\n").unwrap();
    let files = [("SKILL.md", old), ("helper.md", "old helper\n")]
        .iter()
        .map(|(n, t)| ((*n).to_owned(), format!("{:016x}", guidance::fnv1a64(t.as_bytes()))))
        .collect();
    fs::write(dir.join(MARKER), serde_json::to_vec(&Marker { version: "0.1.0".into(), files }).unwrap()).unwrap();

    let status = guidance::folder_status(&dir).unwrap();
    assert_eq!(status.state, FolderState::Older { version: "0.1.0".into() });
    assert_eq!(status.extra, ["notes.md"]);
    assert_eq!(status.no_longer_shipped, ["helper.md"]);
    install(&dir, false).unwrap();
    assert_eq!(skill(&dir), SKILL_FILES[0].1);
    let marker: Marker = serde_json::from_slice(&fs::read(dir.join(MARKER)).unwrap()).unwrap();
    assert_eq!(marker, Marker::of_this_copy(V), "the marker is this copy's");
    assert_eq!(fs::read_to_string(dir.join("notes.md")).unwrap(), "mine\n");
    assert_eq!(fs::read_to_string(dir.join("helper.md")).unwrap(), "old helper\n", "nothing is deleted");
}

// [skill-install#3]
#[test]
fn an_edited_or_hand_copied_folder_is_refused_unless_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = guidance::skill_folder(tmp.path());
    install(&dir, false).unwrap();
    fs::write(dir.join("SKILL.md"), "edited by hand\n").unwrap();
    assert_eq!(guidance::folder_status(&dir).unwrap().state, FolderState::Edited);
    assert!(matches!(guidance::folder_plan(&dir, V, false), Err(PlanError::Folder(FolderRefusal::Edited(_)))));
    install(&dir, true).unwrap();
    assert_eq!(skill(&dir), SKILL_FILES[0].1);

    let hand = guidance::skill_folder(&tmp.path().join("p"));
    fs::create_dir_all(&hand).unwrap();
    fs::write(hand.join("SKILL.md"), "---\nname: ddc\nmetadata:\n  version: \"0.30.0\"\n---\n").unwrap();
    assert_eq!(guidance::folder_status(&hand).unwrap().state, FolderState::NotOurs { version: Some("0.30.0".into()) });
    let refusal = guidance::folder_plan(&hand, V, false).unwrap_err();
    assert!(refusal.to_string().contains("not installed by ddc") && refusal.to_string().contains("--replace"));
    install(&hand, true).unwrap();
    assert_eq!(guidance::folder_status(&hand).unwrap().state, FolderState::Current);
}

// [skill-install#3]: an install stopped before its marker counts as unedited.
#[test]
fn a_file_equal_to_this_copy_counts_as_unedited() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = guidance::skill_folder(tmp.path());
    install(&dir, false).unwrap();
    let mut marker = Marker::of_this_copy("0.1.0");
    marker.files.insert("SKILL.md".into(), "0000000000000000".into());
    fs::write(dir.join(MARKER), serde_json::to_vec(&marker).unwrap()).unwrap();
    assert_eq!(guidance::folder_status(&dir).unwrap().state, FolderState::Older { version: "0.1.0".into() });
    assert_eq!(install(&dir, false).unwrap(), [MARKER]);
}

// [skill-install#3]: a marker that does not parse is not ours; a listed file
// that is gone is an edit; a shipped file the marker does not list, and that
// differs, is someone's own.
#[test]
fn a_broken_marker_a_missing_file_or_an_unlisted_one_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = guidance::skill_folder(tmp.path());
    install(&dir, false).unwrap();
    fs::write(dir.join(MARKER), "not json").unwrap();
    assert_eq!(
        guidance::folder_status(&dir).unwrap().state,
        FolderState::NotOurs { version: Some(env!("CARGO_PKG_VERSION").into()) }
    );

    let dir = guidance::skill_folder(&tmp.path().join("b"));
    install(&dir, false).unwrap();
    fs::rename(dir.join("SKILL.md"), tmp.path().join("gone.md")).unwrap();
    assert_eq!(guidance::folder_status(&dir).unwrap().state, FolderState::Edited);

    let dir = guidance::skill_folder(&tmp.path().join("c"));
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), "the person's own\n").unwrap();
    fs::write(
        dir.join(MARKER),
        serde_json::to_vec(&Marker { version: "0.1.0".into(), files: Default::default() }).unwrap(),
    )
    .unwrap();
    assert_eq!(guidance::folder_status(&dir).unwrap().state, FolderState::Edited);
    assert!(matches!(guidance::folder_plan(&dir, V, false), Err(PlanError::Folder(FolderRefusal::Edited(_)))));
}

// [skill-install#2]: a temporary file an interrupted install left is extra.
#[test]
fn a_leftover_temporary_file_is_extra() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = guidance::skill_folder(tmp.path());
    install(&dir, false).unwrap();
    fs::write(dir.join(".SKILL.md.tmp-4242"), "half").unwrap();
    let status = guidance::folder_status(&dir).unwrap();
    assert_eq!((status.state, status.extra), (FolderState::Current, vec![".SKILL.md.tmp-4242".to_owned()]));
    assert!(dir.join(".SKILL.md.tmp-4242").exists());
}

// [skill-install#4]
#[cfg(unix)]
#[test]
fn links_are_refused_even_with_replace() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    fs::create_dir_all(&real).unwrap();
    let dir = guidance::skill_folder(tmp.path());
    fs::create_dir_all(dir.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, &dir).unwrap();
    assert_eq!(guidance::folder_status(&dir).unwrap().state, FolderState::Link);
    assert!(matches!(guidance::folder_plan(&dir, V, true), Err(PlanError::Folder(FolderRefusal::Link(_)))));
    assert!(fs::read_dir(&real).unwrap().next().is_none(), "nothing written through the link");

    let file = tmp.path().join("AGENTS.md");
    let target = tmp.path().join("other.md");
    fs::write(&target, "other\n").unwrap();
    std::os::unix::fs::symlink(&target, &file).unwrap();
    assert_eq!(guidance::agents_md_status(&file).unwrap().state, FileState::Link);
    assert!(matches!(guidance::agents_md_plan(&file), Err(PlanError::File(guidance::FileRefusal::Link(_)))));
    assert_eq!(fs::read_to_string(&target).unwrap(), "other\n");

    // A link inside the folder, where ddc writes.
    for name in ["SKILL.md", MARKER] {
        let dir = guidance::skill_folder(&tmp.path().join(name));
        install(&dir, false).unwrap();
        fs::rename(dir.join(name), tmp.path().join(format!("moved-{name}"))).unwrap();
        std::os::unix::fs::symlink(&target, dir.join(name)).unwrap();
        assert_eq!(guidance::folder_status(&dir).unwrap().state, FolderState::Link, "{name}");
        assert!(guidance::folder_plan(&dir, V, true).is_err(), "{name}");
    }
    assert_eq!(fs::read_to_string(&target).unwrap(), "other\n");
}

// [skill-install#2]: a replaced file keeps its mode.
#[cfg(unix)]
#[test]
fn a_replaced_file_keeps_its_mode() {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir().unwrap();
    let dir = guidance::skill_folder(tmp.path());
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), "older\n").unwrap();
    fs::set_permissions(dir.join("SKILL.md"), fs::Permissions::from_mode(0o640)).unwrap();
    let marker = Marker {
        version: "0.1.0".into(),
        files: [("SKILL.md".into(), format!("{:016x}", guidance::fnv1a64(b"older\n")))].into(),
    };
    fs::write(dir.join(MARKER), serde_json::to_vec(&marker).unwrap()).unwrap();
    install(&dir, false).unwrap();
    assert_eq!(fs::metadata(dir.join("SKILL.md")).unwrap().permissions().mode() & 0o777, 0o640);

    let file = tmp.path().join("AGENTS.md");
    fs::write(&file, "# Notes\n").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    let bytes = guidance::agents_md_plan(&file).unwrap().unwrap();
    guidance::write_agents_md(&file, &bytes).unwrap();
    assert_eq!(fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
}

fn plan_of(text: Option<&str>) -> Result<Option<String>, PlanError> {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("AGENTS.md");
    if let Some(text) = text {
        fs::write(&file, text).unwrap();
    }
    guidance::agents_md_plan(&file)
}

fn block(body: &str) -> String {
    format!("{BEGIN}\n{body}\n{END}\n")
}

// [skill-install#6]
#[test]
fn a_malformed_agents_md_is_refused() {
    for (text, why) in [
        (format!("{BEGIN}\nno end\n"), "no end line"),
        (format!("{END}\n{BEGIN}\n"), "no begin line before it"),
        (format!("{}{}", block("a"), block("b")), "more than one begin line"),
        (format!("{BEGIN}\nx\n{END}\n{END}\n"), "more than one end line"),
        (format!("# Notes\r\n{}", block("a")), "CRLF"),
    ] {
        let err = plan_of(Some(&text)).unwrap_err().to_string();
        assert!(err.contains(why), "{text:?}: {err}");
    }
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("AGENTS.md");
    fs::write(&file, format!("{BEGIN}\nno end\n")).unwrap();
    assert_eq!(guidance::agents_md_status(&file).unwrap().state, FileState::Malformed);
    // A marker among other text is ordinary text.
    let inline = format!("See `{BEGIN}` in the docs.\n");
    assert_eq!(plan_of(Some(&inline)).unwrap().unwrap(), format!("{inline}\n{SNIPPET}"));
}

// [skill-install#7]
#[test]
fn the_block_is_replaced_and_everything_else_kept() {
    let text = format!("# Project\n\nbefore\n{}after\n", block("an older block"));
    assert_eq!(plan_of(Some(&text)).unwrap().unwrap(), format!("# Project\n\nbefore\n{SNIPPET}after\n"));
    let last = format!("before\n{BEGIN}\nold\n{END}");
    assert_eq!(
        plan_of(Some(&last)).unwrap().unwrap(),
        format!("before\n{SNIPPET}"),
        "the newline is the one byte added"
    );
    assert_eq!(plan_of(Some(&format!("x\n{SNIPPET}y\n"))).unwrap(), None, "this copy in place");
}

// [skill-install#7]
#[test]
fn a_block_is_appended_after_one_empty_line() {
    assert_eq!(plan_of(Some("text")).unwrap().unwrap(), format!("text\n\n{SNIPPET}"));
    assert_eq!(plan_of(Some("text\n")).unwrap().unwrap(), format!("text\n\n{SNIPPET}"));
    assert_eq!(plan_of(Some("text\n\n")).unwrap().unwrap(), format!("text\n\n{SNIPPET}"), "no second empty line");
    assert_eq!(plan_of(Some("")).unwrap().unwrap(), SNIPPET);
    assert_eq!(plan_of(None).unwrap().unwrap(), SNIPPET);
}

// [skill-install#1]
#[test]
fn an_agents_md_needs_its_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let err = guidance::agents_md_plan(&tmp.path().join("missing/AGENTS.md")).unwrap_err();
    assert!(err.to_string().contains("its folder does not exist"), "{err}");
}

// [skill-install#10]
#[test]
fn an_agents_md_reports_its_state() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("AGENTS.md");
    let state = || guidance::agents_md_status(&file).unwrap().state;
    assert_eq!(state(), FileState::Absent);
    fs::write(&file, "# Notes\n").unwrap();
    assert_eq!(state(), FileState::Absent, "no block");
    fs::write(&file, format!("# Notes\n{}", block("older"))).unwrap();
    assert_eq!(state(), FileState::Differs);
    fs::write(&file, format!("# Notes\n{SNIPPET}")).unwrap();
    assert_eq!(state(), FileState::Current);
}

// [skill-install#5]: `metadata.version` as `make bump` writes it.
#[test]
fn the_metadata_version_is_read_from_the_frontmatter() {
    assert_eq!(guidance::metadata_version(SKILL_FILES[0].1).as_deref(), Some(env!("CARGO_PKG_VERSION")));
    assert_eq!(guidance::metadata_version("---\nname: ddc\n---\nmetadata:\n  version: \"1\"\n"), None);
    assert_eq!(guidance::metadata_version("---\nmetadata:\n  version: '2'\nname: ddc\n---\n").as_deref(), Some("2"));
}

// [skill-install#9] [skill-install#10]: the report's JSON.
#[test]
fn the_status_report_names_each_state() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = guidance::skill_folder(tmp.path());
    let file = tmp.path().join("AGENTS.md");
    let report = guidance::SkillReport::new(
        vec![guidance::folder_status(&dir).unwrap()],
        Some(guidance::agents_md_status(&file).unwrap()),
    );
    let v = serde_json::to_value(&report).unwrap();
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["skills"][0]["state"], "absent");
    assert_eq!(v["agents_md"]["state"], "absent");
    let hand = guidance::FolderStatus {
        path: dir.clone(),
        family: guidance::Family::ClaudeCode,
        state: FolderState::NotOurs { version: None },
        extra: vec![],
        no_longer_shipped: vec![],
    };
    assert_eq!(serde_json::to_value(&hand).unwrap()["state"], "not-ours");
}

#[test]
#[allow(clippy::disallowed_methods)] // DDC_UPDATE_SCHEMA regenerates the committed file
fn committed_skill_schema_is_current() {
    let schema = serde_json::to_string_pretty(&schemars::schema_for!(guidance::SkillReport)).unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/skill.schema.json");
    if std::env::var_os("DDC_UPDATE_SCHEMA").is_some() {
        fs::write(&path, &schema).unwrap();
    }
    let committed = fs::read_to_string(&path).unwrap_or_default();
    assert!(committed == schema, "docs/skill.schema.json is stale: run DDC_UPDATE_SCHEMA=1 cargo test");
}

// [skill-install#1]: the shared folder is a skill folder like Claude Code's.
#[test]
fn the_shared_agents_folder_is_one_more_skill_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let shared = guidance::Family::Agents.folder(tmp.path());
    assert!(shared.ends_with(".agents/skills/ddc"), "{}", shared.display());
    assert_eq!(guidance::Family::ClaudeCode.folder(tmp.path()), guidance::skill_folder(tmp.path()));
    install(&shared, false).unwrap();
    let status = guidance::folder_status(&shared).unwrap();
    assert_eq!((status.family, &status.state), (guidance::Family::Agents, &FolderState::Current));
    let v = serde_json::to_value(&status).unwrap();
    assert_eq!(v["family"], "agents");
    let claude = guidance::folder_status(&guidance::skill_folder(tmp.path())).unwrap();
    assert_eq!((claude.family, claude.state), (guidance::Family::ClaudeCode, FolderState::Absent));
}
