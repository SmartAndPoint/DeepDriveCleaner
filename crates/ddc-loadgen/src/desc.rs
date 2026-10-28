//! A tree as data. A profile writes one [`Desc`]; [`crate::build`] creates
//! it on disk and [`crate::manifest`] derives what a scan of it must find.
//! Paths are tree-relative bytes joined with `/`: names need not be UTF-8.

use std::collections::BTreeSet;

/// Every file that carries bytes is a multiple of this long, so its
/// allocated size is the same on every file system.
pub const BLOCK: u64 = 4096;

/// Modification time of an entry. `Old` is a fixed date, far older than the
/// scan's `min_age`; `Fresh` is the build time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Age {
    Old,
    Fresh,
}

/// The fixed date of [`Age::Old`]: 2024-01-01T00:00:00Z.
pub const OLD_SECS: i64 = 1_704_067_200;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Dir,
    /// `blocks` × [`BLOCK`] bytes of non-zero content; 0 is an empty file.
    File {
        blocks: u32,
    },
    /// A short text; `{tree}` is replaced by the tree's absolute path when
    /// built. It allocates one block, or none when empty.
    Text(String),
    Symlink(Vec<u8>),
    /// Another name for the file at this tree-relative path.
    HardLink(Vec<u8>),
}

#[derive(Debug, Clone)]
pub struct Node {
    pub path: Vec<u8>,
    pub kind: Kind,
    pub age: Age,
}

/// A finding the scan must report: where, and by which rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planted {
    pub path: Vec<u8>,
    pub rule: &'static str,
}

/// What the file system the tree is built on accepts (profile `names`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct FsCaps {
    /// `a` and `A` name the same entry.
    pub case_insensitive: bool,
    /// NFC and NFD spellings name the same entry.
    pub normalization_insensitive: bool,
    /// Names that are not UTF-8 can be created (APFS refuses them).
    pub invalid_utf8: bool,
}

/// A whole tree. Parents are created on demand; nodes keep insertion order
/// until [`Desc::finish`] sorts them depth-first.
#[derive(Debug, Clone, Default)]
pub struct Desc {
    /// Tree-relative home folder, or `None` for an empty home beside the tree.
    pub home: Option<Vec<u8>>,
    pub nodes: Vec<Node>,
    pub planted: Vec<Planted>,
    /// Folders the scan never opens (cloud folders): not counted, nor anything in them.
    pub unopened: Vec<Vec<u8>>,
    /// What the file system accepted, for the profiles whose tree depends on it.
    pub caps: Option<FsCaps>,
    dirs: BTreeSet<Vec<u8>>,
}

impl Desc {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// A directory and any missing parents.
    pub fn dir(&mut self, path: impl AsRef<[u8]>) {
        let path = path.as_ref();
        if path.is_empty() || self.dirs.contains(path) {
            return;
        }
        if let Some(parent) = parent(path) {
            self.dir(parent);
        }
        self.dirs.insert(path.to_vec());
        self.nodes.push(Node { path: path.to_vec(), kind: Kind::Dir, age: Age::Old });
    }

    fn entry(&mut self, path: &[u8], kind: Kind) {
        if let Some(parent) = parent(path) {
            self.dir(parent);
        }
        debug_assert!(!self.dirs.contains(path), "{} is a directory", String::from_utf8_lossy(path));
        self.nodes.push(Node { path: path.to_vec(), kind, age: Age::Old });
    }

    pub fn file(&mut self, path: impl AsRef<[u8]>, blocks: u32) {
        self.entry(path.as_ref(), Kind::File { blocks });
    }

    pub fn text(&mut self, path: impl AsRef<[u8]>, text: &str) {
        self.entry(path.as_ref(), Kind::Text(text.to_owned()));
    }

    pub fn symlink(&mut self, path: impl AsRef<[u8]>, target: impl AsRef<[u8]>) {
        self.entry(path.as_ref(), Kind::Symlink(target.as_ref().to_vec()));
    }

    /// A second name for `target`, a file of the tree; links are built after every file.
    pub fn hardlink(&mut self, path: impl AsRef<[u8]>, target: impl AsRef<[u8]>) {
        self.entry(path.as_ref(), Kind::HardLink(target.as_ref().to_vec()));
    }

    /// Marks the newest entry at `path` as written at build time.
    pub fn fresh(&mut self, path: impl AsRef<[u8]>) {
        let path = path.as_ref();
        if let Some(n) = self.nodes.iter_mut().rev().find(|n| n.path == path) {
            n.age = Age::Fresh;
        }
    }

    pub fn plant(&mut self, path: impl AsRef<[u8]>, rule: &'static str) {
        self.planted.push(Planted { path: path.as_ref().to_vec(), rule });
    }

    /// Sorts the nodes depth-first by name, so every parent comes right
    /// before its subtree; the builder relies on it.
    pub fn finish(mut self) -> Self {
        self.nodes.sort_by(|a, b| a.path.split(|&c| c == b'/').cmp(b.path.split(|&c| c == b'/')));
        self.planted.sort_by(|a, b| a.path.cmp(&b.path));
        self
    }
}

/// The parent of a tree-relative path; `None` at the top.
pub fn parent(path: &[u8]) -> Option<&[u8]> {
    path.iter().rposition(|&c| c == b'/').map(|i| &path[..i])
}

/// The last component of a tree-relative path.
pub fn name(path: &[u8]) -> &[u8] {
    path.iter().rposition(|&c| c == b'/').map_or(path, |i| &path[i + 1..])
}

/// `path` joined to `rel`, either of which may be empty.
pub fn join(path: &[u8], rel: &[u8]) -> Vec<u8> {
    match (path.is_empty(), rel.is_empty()) {
        (true, _) => rel.to_vec(),
        (_, true) => path.to_vec(),
        _ => [path, b"/", rel].concat(),
    }
}

/// Whether `path` is `dir` or lies inside it.
pub fn within(path: &[u8], dir: &[u8]) -> bool {
    dir.is_empty() || path == dir || (path.starts_with(dir) && path.get(dir.len()) == Some(&b'/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_orders_parents_before_their_subtrees() {
        let mut d = Desc::new();
        d.file("a/b", 0);
        d.file("a-c", 0);
        d.file("a/a/x", 0);
        let paths: Vec<_> = d.finish().nodes.into_iter().map(|n| String::from_utf8(n.path).unwrap()).collect();
        assert_eq!(paths, ["a", "a/a", "a/a/x", "a/b", "a-c"]);
    }

    #[test]
    fn within_matches_whole_components() {
        assert!(within(b"a/b", b"a"));
        assert!(!within(b"ab", b"a"));
        assert!(within(b"x", b""));
    }
}
