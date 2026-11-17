//! The `ddc` block in an `AGENTS.md` (spec "Agent skill install", contracts 6 and 7,
//! spec "Agent skill install", contract 10).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;

use super::SNIPPET;

/// The block's first line.
pub const BEGIN: &str = "<!-- ddc:begin -->";
/// The block's last line.
pub const END: &str = "<!-- ddc:end -->";

/// What an `AGENTS.md` holds of the block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum FileState {
    /// No file, or no block in it.
    Absent,
    /// The block is this copy's.
    Current,
    /// The block differs from this copy's.
    Differs,
    /// What `install` refuses: an unpaired or second marker line, or a CRLF.
    Malformed,
    /// The file is a symbolic link.
    Link,
}

/// An `AGENTS.md`'s state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct FileStatus {
    #[serde(serialize_with = "crate::json_path::path")]
    pub path: PathBuf,
    pub state: FileState,
}

/// Why `install` writes nothing into an `AGENTS.md`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FileRefusal {
    #[error("{0} is a symbolic link")]
    Link(PathBuf),
    #[error("{0}: {1}")]
    Malformed(PathBuf, &'static str),
    #[error("{0}: its folder does not exist")]
    NoParent(PathBuf),
}

/// Where the block lies in `text`: the byte range from its begin line to the
/// end of its end line, the newline included when there is one.
fn find_block(text: &str) -> Result<Option<(usize, usize)>, &'static str> {
    if text.contains('\r') {
        return Err("it has CRLF line endings, which ddc does not edit yet");
    }
    let mut begin = None;
    let mut end = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let bare = line.strip_suffix('\n').unwrap_or(line);
        if bare == BEGIN {
            if begin.is_some() {
                return Err("it has more than one begin line");
            }
            begin = Some(offset);
        } else if bare == END {
            if end.is_some() {
                return Err("it has more than one end line");
            }
            if begin.is_none() {
                return Err("it has an end line with no begin line before it");
            }
            end = Some(offset + line.len());
        }
        offset += line.len();
    }
    match (begin, end) {
        (None, None) => Ok(None),
        (Some(b), Some(e)) => Ok(Some((b, e))),
        _ => Err("it has a begin line with no end line after it"),
    }
}

fn read(path: &Path) -> io::Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
        Ok(_) => {}
    }
    let bytes = fs::read(path)?;
    String::from_utf8(bytes).map(Some).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "not UTF-8"))
}

/// The state of `path` against this copy's block. Reads only.
pub fn agents_md_status(path: &Path) -> io::Result<FileStatus> {
    let status = |state| FileStatus { path: path.to_owned(), state };
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Ok(status(FileState::Link));
    }
    let Some(text) = read(path)? else {
        return Ok(status(FileState::Absent));
    };
    Ok(status(match find_block(&text) {
        Err(_) => FileState::Malformed,
        Ok(None) => FileState::Absent,
        Ok(Some((b, e))) if text[b..e] == *SNIPPET => FileState::Current,
        Ok(Some(_)) => FileState::Differs,
    }))
}

/// The bytes `install` would write to `path`, or `None` when its block is this
/// copy's already, or why it refuses. Writes nothing.
pub fn agents_md_plan(path: &Path) -> Result<Option<String>, super::PlanError> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(FileRefusal::Link(path.to_owned()).into());
    }
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    if !parent.is_dir() {
        return Err(FileRefusal::NoParent(path.to_owned()).into());
    }
    let text = read(path)?.unwrap_or_default();
    let new = match find_block(&text).map_err(|why| FileRefusal::Malformed(path.to_owned(), why))? {
        Some((b, e)) => format!("{}{SNIPPET}{}", &text[..b], &text[e..]),
        None if text.is_empty() => SNIPPET.to_owned(),
        None => {
            let mut new = text.clone();
            if !new.ends_with('\n') {
                new.push('\n');
            }
            if new != "\n" && !new.ends_with("\n\n") {
                new.push('\n');
            }
            new.push_str(SNIPPET);
            new
        }
    };
    Ok((new != text).then_some(new))
}

/// Writes `bytes` to `path` atomically, keeping its mode (spec "Agent skill install", contract 7).
pub fn write_agents_md(path: &Path, bytes: &str) -> io::Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(io::Error::other(format!("{} became a symbolic link", path.display())));
    }
    crate::store::write_atomic(path, bytes.as_bytes()).map(|_| ())
}
