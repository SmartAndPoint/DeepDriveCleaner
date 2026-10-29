//! Matching of specialized rules against one directory. The walk matches on
//! the directory listing it already has; the executor's re-check matches on
//! a fresh filesystem view — through the same function.

use std::ffi::OsStr;
use std::fs;
use std::io::Read as _;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use super::Matcher;
use crate::scan::{DirListing, EntryKind};

const CACHEDIR_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// What a matcher needs to know about a directory.
pub(crate) trait DirView {
    fn path(&self) -> &Path;
    fn has_child(&self, name: &str) -> bool;
    fn has_child_dir(&self, name: &str) -> bool;
}

pub(crate) struct ListingView<'a> {
    pub path: &'a Path,
    pub listing: &'a DirListing,
}

impl DirView for ListingView<'_> {
    fn path(&self) -> &Path {
        self.path
    }
    fn has_child(&self, name: &str) -> bool {
        self.listing.entries.iter().any(|e| e.name == OsStr::new(name))
    }
    fn has_child_dir(&self, name: &str) -> bool {
        self.listing.entries.iter().any(|e| e.kind == EntryKind::Dir && e.name == OsStr::new(name))
    }
}

/// Live view of a directory (no symlink following).
pub(crate) struct FsView {
    path: PathBuf,
    names: Vec<(std::ffi::OsString, bool)>,
}

impl FsView {
    pub fn open(path: &Path) -> Option<Self> {
        if !fs::symlink_metadata(path).ok()?.is_dir() {
            return None;
        }
        let names = crate::stats::read_dir(path)
            .ok()?
            .filter_map(Result::ok)
            .map(|e| (e.file_name(), e.file_type().is_ok_and(|t| t.is_dir())))
            .collect();
        Some(Self { path: path.to_path_buf(), names })
    }
}

impl DirView for FsView {
    fn path(&self) -> &Path {
        &self.path
    }
    fn has_child(&self, name: &str) -> bool {
        self.names.iter().any(|(n, _)| n == OsStr::new(name))
    }
    fn has_child_dir(&self, name: &str) -> bool {
        self.names.iter().any(|(n, d)| *d && n == OsStr::new(name))
    }
}

fn exists_no_follow(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// `true` when `dir` matches a specialized matcher. Known-path matchers are
/// resolved by the catalog, not here.
pub(crate) fn matches(m: &Matcher, dir: &dyn DirView, home: &Path) -> bool {
    crate::stats::add(crate::stats::Counter::RuleMatchAttempts, 1);
    let path = dir.path();
    let name = path.file_name().unwrap_or_default();
    match *m {
        Matcher::Name { names, siblings, children_any } => {
            names.iter().any(|n| name == OsStr::new(n))
                && (siblings.is_empty()
                    || path.parent().is_some_and(|p| siblings.iter().any(|s| exists_no_follow(&p.join(s)))))
                && (children_any.is_empty() || children_any.iter().any(|c| dir.has_child_dir(c)))
        }
        Matcher::ContainsFile(file) => dir.has_child(file),
        Matcher::NameSuffix(suffix) => name.to_str().is_some_and(|n| n.len() > suffix.len() && n.ends_with(suffix)),
        Matcher::CmakeBuild => {
            let conventional = name.to_str().is_some_and(|n| {
                n.starts_with("build") || n.starts_with("cmake-build-") || matches!(n, "out" | "_build" | ".build")
            });
            conventional
                && dir.has_child("CMakeCache.txt")
                && !dir.has_child("CMakeLists.txt")
                && cmake_out_of_source(path)
        }
        Matcher::NameContains(text) => name.as_bytes().windows(text.len()).any(|w| w == text.as_bytes()),
        Matcher::CacheDirTag => dir.has_child("CACHEDIR.TAG") && cachedir_tag_valid(&path.join("CACHEDIR.TAG")),
        Matcher::Under { name: want, prefixes } => {
            name == OsStr::new(want)
                && path.strip_prefix(home).is_ok_and(|rel| prefixes.iter().any(|p| rel.starts_with(p)))
        }
        Matcher::ChildHolding { parents, file } => {
            let in_parent = path
                .parent()
                .and_then(|p| p.strip_prefix(home).ok())
                .is_some_and(|rel| parents.iter().any(|p| rel == Path::new(p)));
            in_parent
                && file.split('/').next().is_some_and(|top| dir.has_child(top))
                && fs::symlink_metadata(path.join(file)).is_ok_and(|m| m.is_file())
        }
        Matcher::KnownPaths(_)
        | Matcher::KnownRegion(_)
        | Matcher::KnownChildren(_)
        | Matcher::NoTraverse(_)
        | Matcher::Synthetic => false,
    }
}

/// Out-of-source CMake build: `CMAKE_HOME_DIRECTORY` exists and lies outside
/// the directory (an in-source or `-S src -B .` tree is never a build dir).
fn cmake_out_of_source(dir: &Path) -> bool {
    let Ok(text) = fs::read_to_string(dir.join("CMakeCache.txt")) else { return false };
    let Some(source) = text.lines().find_map(|l| l.strip_prefix("CMAKE_HOME_DIRECTORY:")) else { return false };
    let source = source.split_once('=').map_or(source, |(_, v)| v).trim();
    match (Path::new(source).canonicalize(), dir.canonicalize()) {
        (Ok(src), Ok(dir)) => !src.starts_with(&dir),
        _ => false, // cannot tell: never a build dir
    }
}

pub(crate) fn cachedir_tag_valid(tag: &Path) -> bool {
    if !fs::symlink_metadata(tag).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let mut head = [0u8; CACHEDIR_SIGNATURE.len()];
    fs::File::open(tag).and_then(|mut f| f.read_exact(&mut head)).is_ok() && head == CACHEDIR_SIGNATURE
}

/// Simple glob: at most one `*`.
pub(crate) fn glob_match(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((pre, post)) => name.len() >= pre.len() + post.len() && name.starts_with(pre) && name.ends_with(post),
    }
}
