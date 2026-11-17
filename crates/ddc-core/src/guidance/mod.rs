//! The agent guidance `ddc` installs (spec "Agent skill install", alias
//! `skill-install`; ADR "Agent footprint: worktrees, harness data and bounded
//! consent for agents", §7): the Claude Code skill and the `AGENTS.md` block,
//! built into the binary from `agents/`.
//!
//! It writes and replaces only the files it names and deletes nothing. It is
//! kept apart from [`crate::agents`], the read-only reader of the harnesses'
//! data, which never writes.

mod agents_md;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use agents_md::{
    BEGIN, END, FileRefusal, FileState, FileStatus, agents_md_plan, agents_md_status, write_agents_md,
};

/// The skill's files as this binary ships them: (name in the skill folder, bytes).
pub const SKILL_FILES: &[(&str, &str)] = &[("SKILL.md", include_str!("../../../../agents/claude-code/ddc/SKILL.md"))];

/// The `AGENTS.md` block as this binary ships it, begin and end lines included.
pub const SNIPPET: &str = include_str!("../../../../agents/AGENTS.snippet.md");

/// The marker `install` leaves in the skill folder (spec "Agent skill install", contract 5).
pub const MARKER: &str = ".ddc-skill";

/// The skill folder under `root`: the home folder, or a project's folder.
pub fn skill_folder(root: &Path) -> PathBuf {
    root.join(".claude").join("skills").join("ddc")
}

/// 64-bit FNV-1a: it finds edits and protects against no one (spec "Agent skill install", contract 5).
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

fn hash_hex(bytes: &[u8]) -> String {
    format!("{:016x}", fnv1a64(bytes))
}

/// `.ddc-skill`: who wrote the folder, and each file's hash when it did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub version: String,
    pub files: BTreeMap<String, String>,
}

impl Marker {
    /// The marker of this copy, written by `version`.
    pub fn of_this_copy(version: &str) -> Self {
        let files = SKILL_FILES.iter().map(|(name, text)| ((*name).to_owned(), hash_hex(text.as_bytes()))).collect();
        Self { version: version.to_owned(), files }
    }

    fn to_bytes(&self) -> Vec<u8> {
        let mut text = serde_json::to_string_pretty(self).expect("a marker serialises");
        text.push('\n');
        text.into_bytes()
    }
}

/// What a skill folder is, for `status` and before `install` writes
/// (spec "Agent skill install", contracts 3 and 9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case", tag = "state")]
pub enum FolderState {
    /// No folder.
    Absent,
    /// Unedited, and every file is this copy's.
    Current,
    /// Unedited, written by another version or from another copy.
    Older { version: String },
    /// A file the marker lists is missing or differs from both its hash and this copy.
    Edited,
    /// No marker, or one that does not parse; `version` is a `SKILL.md`'s
    /// `metadata.version` when it has one.
    NotOurs { version: Option<String> },
    /// The folder, a shipped file or the marker is a symbolic link.
    Link,
}

/// A skill folder's state and the files in it that are no shipped file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FolderStatus {
    #[serde(serialize_with = "crate::json_path::path")]
    pub path: PathBuf,
    #[serde(flatten)]
    pub state: FolderState,
    /// Files that are no shipped file and that the old marker does not list
    /// (a temporary file an install left among them).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra: Vec<String>,
    /// Files the old marker lists that this copy no longer ships.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub no_longer_shipped: Vec<String>,
}

/// The version of `docs/skill.schema.json`.
pub const SKILL_SCHEMA_VERSION: u32 = 1;

/// `ddc agent skill status --json` (spec "Agent skill install", contracts 9
/// and 10): docs/skill.schema.json.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SkillReport {
    /// [`SKILL_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The `ddc` that reports, whose copy the states compare with.
    pub ddc_version: String,
    /// The home's skill folder, then the project's with `--project`.
    pub skills: Vec<FolderStatus>,
    /// The `AGENTS.md` of `--agents-md`; absent without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents_md: Option<FileStatus>,
}

impl SkillReport {
    pub fn new(skills: Vec<FolderStatus>, agents_md: Option<FileStatus>) -> Self {
        Self {
            schema_version: SKILL_SCHEMA_VERSION,
            ddc_version: env!("CARGO_PKG_VERSION").to_owned(),
            skills,
            agents_md,
        }
    }
}

fn is_link(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.file_type().is_symlink()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

fn read_if_there(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// `metadata.version` of a `SKILL.md`'s frontmatter, as `make bump` writes it.
pub fn metadata_version(skill_md: &str) -> Option<String> {
    let front = skill_md.strip_prefix("---\n")?.split_once("\n---\n")?.0;
    let rest = format!("\n{front}").split_once("\nmetadata:\n")?.1.to_owned();
    let value = rest.lines().take_while(|l| l.starts_with("  ")).find_map(|l| l.strip_prefix("  version: "))?;
    Some(value.trim().trim_matches(['"', '\'']).to_owned())
}

/// The state of the skill folder `dir` against this copy. Reads only.
pub fn folder_status(dir: &Path) -> io::Result<FolderStatus> {
    let status =
        |state, extra, no_longer_shipped| FolderStatus { path: dir.to_owned(), state, extra, no_longer_shipped };
    let meta = match fs::symlink_metadata(dir) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(status(FolderState::Absent, vec![], vec![])),
        Err(e) => return Err(e),
    };
    if meta.file_type().is_symlink() {
        return Ok(status(FolderState::Link, vec![], vec![]));
    }
    if !meta.is_dir() {
        return Ok(status(FolderState::NotOurs { version: None }, vec![], vec![]));
    }
    for name in SKILL_FILES.iter().map(|(name, _)| *name).chain([MARKER]) {
        if is_link(&dir.join(name))? {
            return Ok(status(FolderState::Link, vec![], vec![]));
        }
    }
    let marker = read_if_there(&dir.join(MARKER))?.and_then(|bytes| serde_json::from_slice::<Marker>(&bytes).ok());
    let mut names: Vec<String> = crate::stats::read_dir(dir)?
        .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect::<io::Result<_>>()?;
    names.sort();
    let shipped = |name: &str| SKILL_FILES.iter().any(|(n, _)| *n == name);
    let listed = |name: &str| marker.as_ref().is_some_and(|m| m.files.contains_key(name));
    let others = names.iter().filter(|n| !shipped(n) && n.as_str() != MARKER);
    let extra = others.clone().filter(|n| !listed(n)).cloned().collect();
    let no_longer_shipped = others.filter(|n| listed(n)).cloned().collect();
    let Some(marker) = marker else {
        let skill = read_if_there(&dir.join("SKILL.md"))?;
        let version = skill.and_then(|bytes| metadata_version(&String::from_utf8_lossy(&bytes)));
        return Ok(status(FolderState::NotOurs { version }, extra, no_longer_shipped));
    };
    let mut every_file_ours = true;
    for (name, hash) in &marker.files {
        let this_copy = SKILL_FILES.iter().find(|(n, _)| n == name).map(|(_, text)| text.as_bytes());
        match read_if_there(&dir.join(name))? {
            Some(bytes) if hash_hex(&bytes) == *hash || this_copy == Some(&bytes[..]) => {}
            _ => return Ok(status(FolderState::Edited, extra, no_longer_shipped)),
        }
    }
    for (name, text) in SKILL_FILES {
        let bytes = read_if_there(&dir.join(name))?;
        // A shipped file the marker does not list, there and not this copy's,
        // is someone's own: never written over without --replace.
        if bytes.is_some() && !marker.files.contains_key(*name) && bytes.as_deref() != Some(text.as_bytes()) {
            return Ok(status(FolderState::Edited, extra, no_longer_shipped));
        }
        if bytes.as_deref() != Some(text.as_bytes()) {
            every_file_ours = false;
        }
    }
    let state = if every_file_ours && marker.files == Marker::of_this_copy(&marker.version).files {
        FolderState::Current
    } else {
        FolderState::Older { version: marker.version }
    };
    Ok(status(state, extra, no_longer_shipped))
}

/// Why `install` writes nothing into a skill folder (spec "Agent skill install", contracts 3 and 4).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FolderRefusal {
    #[error("{0} is a symbolic link, or holds one where ddc writes")]
    Link(PathBuf),
    #[error("{0} was not installed by ddc; --replace writes over it")]
    NotOurs(PathBuf),
    #[error("{0} was edited after ddc installed it; --replace writes over it")]
    Edited(PathBuf),
}

/// What `install` would write into a skill folder: the shipped files that
/// differ, then the marker. Empty when this copy is in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderPlan {
    pub status: FolderStatus,
    pub writes: Vec<PathBuf>,
    marker: Marker,
}

/// Works out what `install` would write into `dir`, or why it refuses.
/// Writes nothing.
pub fn folder_plan(dir: &Path, version: &str, replace: bool) -> Result<FolderPlan, PlanError> {
    let status = folder_status(dir)?;
    match &status.state {
        FolderState::Link => return Err(FolderRefusal::Link(dir.to_owned()).into()),
        FolderState::NotOurs { .. } if !replace => return Err(FolderRefusal::NotOurs(dir.to_owned()).into()),
        FolderState::Edited if !replace => return Err(FolderRefusal::Edited(dir.to_owned()).into()),
        _ => {}
    }
    let marker = Marker::of_this_copy(version);
    if status.state == FolderState::Current {
        return Ok(FolderPlan { status, writes: vec![], marker });
    }
    let mut writes = Vec::new();
    for (name, text) in SKILL_FILES {
        let path = dir.join(name);
        if read_if_there(&path)?.as_deref() != Some(text.as_bytes()) {
            writes.push(path);
        }
    }
    writes.push(dir.join(MARKER));
    Ok(FolderPlan { status, writes, marker })
}

/// Writes what [`folder_plan`] found: each shipped file that differs, then
/// the marker last, each atomically, keeping its mode (spec "Agent skill install", contract 2).
pub fn write_folder(plan: &FolderPlan) -> io::Result<()> {
    if is_link(&plan.status.path)? {
        return Err(io::Error::other(format!("{} became a symbolic link", plan.status.path.display())));
    }
    for path in &plan.writes {
        if is_link(path)? {
            return Err(io::Error::other(format!("{} became a symbolic link", path.display())));
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let bytes = match SKILL_FILES.iter().find(|(n, _)| *n == name) {
            Some((_, text)) => text.as_bytes().to_vec(),
            None => plan.marker.to_bytes(),
        };
        crate::store::write_atomic(path, &bytes)?;
    }
    Ok(())
}

/// A plan that could not be made: a refusal, or a file that could not be read.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error(transparent)]
    Folder(#[from] FolderRefusal),
    #[error(transparent)]
    File(#[from] agents_md::FileRefusal),
    #[error(transparent)]
    Io(#[from] io::Error),
}
