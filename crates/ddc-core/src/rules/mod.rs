//! Rule catalog: what a directory is, how risky it is to remove, and how.
//! Matching happens during the walk ([`RuleVisitor`]); effective classes
//! (lock files, app-owned locations, protected toolchain versions, keep
//! markers) are settled after it ([`classify`]).

mod catalog;
mod classify;
pub mod entries;
pub mod listing;
mod matcher;
mod toolchain;
mod visitor;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub use catalog::{DOCKER_DOWN, RULES, SHARED_IDS, rules_of};
pub(crate) use classify::AtOrAbove;
pub use classify::{Classified, LockFiles, Note, classify, effective_class};
pub use visitor::{KEEP_MARKER, LARGE_FILE_BYTES, Pins, RuleVisitor};

use crate::class::{Category, Removal, RiskClass};
use crate::scan::Tag;

/// An app that owns a cache, by its bundle name (`Cursor` for `Cursor.app`): while
/// a process of that app runs, its caches are skipped (ADR §5). Matched by [`owns`].
pub type OwnerApp = &'static str;

/// `owner` names the app bundle `bundle`: the same name, or that name followed by a
/// character that is not a letter or digit, so editions and versions count
/// (`Adobe Photoshop 2025`, `Xcode-beta`, `PyCharm CE`) while `CursorFoo` does not.
pub fn owns(owner: OwnerApp, bundle: &str) -> bool {
    bundle.strip_prefix(owner).is_some_and(|rest| rest.chars().next().is_none_or(|c| !c.is_alphanumeric()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matcher {
    /// Exact paths relative to the home directory.
    KnownPaths(&'static [&'static str]),
    /// Like `KnownPaths`, but rules keep working inside and the rest belongs here.
    KnownRegion(&'static [&'static str]),
    /// Every child directory of this home-relative directory.
    KnownChildren(&'static str),
    /// Never traversed (cloud folders).
    NoTraverse(&'static [&'static str]),
    /// Directory name, optionally with sibling files in the parent and/or
    /// one of the given children.
    Name { names: &'static [&'static str], siblings: &'static [&'static str], children_any: &'static [&'static str] },
    /// Any directory containing this file (`pyvenv.cfg`).
    ContainsFile(&'static str),
    /// Name ends with this text (`.app` bundles).
    NameSuffix(&'static str),
    /// CMake binary dir: `CMakeCache.txt`, no `CMakeLists.txt`, and
    /// `CMAKE_HOME_DIRECTORY` is not the directory itself.
    CmakeBuild,
    /// Name contains this text (tombstones).
    NameContains(&'static str),
    /// Valid `CACHEDIR.TAG`.
    CacheDirTag,
    /// Directories named `name` under these home-relative prefixes.
    Under { name: &'static str, prefixes: &'static [&'static str] },
    /// A child directory of one of these home-relative folders that holds
    /// `file`, a path relative to it (`conda-meta/history`: a conda
    /// environment). Never the folder itself, nor anything deeper.
    ChildHolding { parents: &'static [&'static str], file: &'static str },
    /// Assigned by the visitor (exclusions, `.ddc-keep`).
    Synthetic,
}

#[derive(Debug)]
pub struct Rule {
    /// Stable id: appears in JSON, the journal and the baseline.
    pub id: &'static str,
    pub category: Category,
    pub class: RiskClass,
    pub removal: Removal,
    pub matcher: Matcher,
    /// Environments: `rebuildable` only when one of these exists in the
    /// project (`*` matches any text). `None` = not lock-gated.
    pub lockfiles: Option<&'static [&'static str]>,
    pub owners: &'static [OwnerApp],
    /// How to get it back, shown in plans.
    pub hint: &'static str,
    /// Shown in reports. Unlisted rules only stop other rules from matching inside.
    pub listed: bool,
    /// Where APFS clones are expected, so the private size is measured.
    pub clones: Clones,
    /// What its verdict needs the walk to have visited.
    pub scope: Scope,
}

/// What a rule's verdict needs the walk to have visited (safety ADR "Cleanup
/// safety model", amendment of 2026-10-24): a clean re-checks its list with
/// the walk its rules need. Every rule has one and it is matched exhaustively,
/// so a new rule that reads outside its item cannot be missed silently. A
/// path a rule stats or reads on its own does not count: the pruned walk
/// makes the same call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The item, its parent and its ancestors.
    Item,
    /// Also the other children of its parent, whose newest is kept.
    Siblings,
    /// Files anywhere in the scan roots: pin files in any project protect a version.
    Tree,
}

/// APFS clones come almost only from uv and pnpm; elsewhere freed ≈ allocated
/// and the (expensive) private size is not measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clones {
    Never,
    Always,
    /// A venv whose `pyvenv.cfg` was written by uv.
    UvVenv,
    /// `node_modules` installed by pnpm (`.pnpm` or `.modules.yaml` inside) or
    /// by bun (`bun.lock`/`bun.lockb` next to it): both clone on macOS.
    PnpmModules,
}

/// Id of the synthetic rule for user-protected paths.
pub const KEPT: &str = "user.kept";
/// What an interrupted clean leaves behind (`.ddc-deleting-*`).
pub const TOMBSTONE: &str = "ddc.tombstone";

/// Directories (relative to `$HOME`) that are never a finding nor deleted
/// themselves (ADR "Cleanup safety model", §6).
pub const PROTECTED_IN_HOME: &[&str] = &[
    "",
    "Documents",
    "Desktop",
    "Pictures",
    "Movies",
    "Music",
    "Downloads",
    "Projects",
    "Library",
    "Library/Caches",
    "Library/Application Support",
    "Library/Containers",
    "Library/Group Containers",
    "Library/Mobile Documents",
    "Library/CloudStorage",
    ".config",
    ".ssh",
    ".gnupg",
    ".local",
    ".cache",
];
/// System directories that are never deleted.
pub const PROTECTED_SYSTEM: &[&str] =
    &["/", "/System", "/Applications", "/Library", "/usr", "/opt", "/opt/homebrew", "/Users"];

/// Manifests that mark a project directory: such a directory is never an artifact.
const PROJECT_MANIFESTS: &[&str] = &[
    "pyproject.toml",
    "setup.py",
    "package.json",
    "Cargo.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "Gemfile",
    "composer.json",
];

/// Why a directory is never opened ([`Catalog::never_open`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeverOpen<'s> {
    /// At or under this skip of the user's.
    UserSkip(&'s PathBuf),
    /// A cloud folder.
    NoTraverse,
}

/// The rule table resolved against a home directory.
#[derive(Debug)]
pub struct Catalog {
    home: PathBuf,
    rules: &'static [Rule],
    known: HashMap<PathBuf, Tag>,
    regions: HashMap<PathBuf, Tag>,
    children_of: HashMap<PathBuf, Tag>,
    no_traverse: HashMap<PathBuf, Tag>,
    /// Ancestors of known paths: never claimed whole (ADR §2, item 3).
    ancestors: HashSet<PathBuf>,
    /// Protected directories: never claimed by specialized or generic rules.
    protected: HashSet<PathBuf>,
    specialized: Vec<Tag>,
    cachedir: Tag,
    kept: Tag,
}

impl Catalog {
    pub fn builtin(home: &Path) -> Self {
        Self::from_rules(RULES, home)
    }

    pub fn from_rules(rules: &'static [Rule], home: &Path) -> Self {
        let mut c = Self {
            home: home.to_path_buf(),
            rules,
            known: HashMap::new(),
            regions: HashMap::new(),
            children_of: HashMap::new(),
            no_traverse: HashMap::new(),
            ancestors: HashSet::new(),
            protected: PROTECTED_IN_HOME.iter().map(|rel| home.join(rel)).collect(),
            specialized: Vec::new(),
            cachedir: Tag::MAX,
            kept: Tag::MAX,
        };
        for (i, rule) in rules.iter().enumerate() {
            let tag = i as Tag;
            let anchor = |map: &mut HashMap<PathBuf, Tag>, rel: &str, ancestors: &mut HashSet<PathBuf>| {
                let path = home.join(rel);
                for a in path.ancestors().skip(1) {
                    if !a.starts_with(home) || a == home {
                        break;
                    }
                    ancestors.insert(a.to_path_buf());
                }
                map.insert(path, tag);
            };
            match rule.matcher {
                Matcher::KnownPaths(paths) => paths.iter().for_each(|p| anchor(&mut c.known, p, &mut c.ancestors)),
                Matcher::KnownRegion(paths) => paths.iter().for_each(|p| anchor(&mut c.regions, p, &mut c.ancestors)),
                Matcher::KnownChildren(parent) => {
                    anchor(&mut c.children_of, parent, &mut c.ancestors);
                    c.ancestors.insert(home.join(parent));
                }
                Matcher::NoTraverse(paths) => paths.iter().for_each(|p| {
                    c.no_traverse.insert(home.join(p), tag);
                }),
                Matcher::CacheDirTag => c.cachedir = tag,
                Matcher::Synthetic if rule.id == KEPT => c.kept = tag,
                Matcher::Synthetic => {}
                _ => c.specialized.push(tag),
            }
        }
        assert!(c.kept != Tag::MAX, "catalog must define {KEPT}");
        c
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Folders the walk never opens, sorted: the cloud folders.
    pub fn no_traverse_paths(&self) -> Vec<&Path> {
        let mut paths: Vec<&Path> = self.no_traverse.keys().map(PathBuf::as_path).collect();
        paths.sort();
        paths
    }

    /// What the walk does with `dir` before anything else, the same for every
    /// walk of the home folder: a user's skip at or above it, or a folder
    /// never traversed, is not opened (ADR "Cleanup safety model", the
    /// cloud folders; story "Settings: folders, types, notifications").
    pub fn never_open<'s>(&self, dir: &Path, user_skips: &'s [PathBuf]) -> Option<NeverOpen<'s>> {
        // A user's skip at or above this directory (above a scan root).
        if let Some(skip) = user_skips.iter().find(|s| dir.starts_with(s)) {
            return Some(NeverOpen::UserSkip(skip));
        }
        self.no_traverse.contains_key(dir).then_some(NeverOpen::NoTraverse)
    }

    pub fn rules(&self) -> &'static [Rule] {
        self.rules
    }

    pub fn rule(&self, tag: Tag) -> &'static Rule {
        &self.rules[tag as usize]
    }

    pub fn by_id(&self, id: &str) -> Option<&'static Rule> {
        self.rules.iter().find(|r| r.id == id)
    }

    pub(crate) fn tag_of(&self, rule: &Rule) -> Option<Tag> {
        self.rules.iter().position(|r| std::ptr::eq(r, rule)).map(|i| i as Tag)
    }

    /// Whether specialized and generic rules may claim this directory: never
    /// a scan root, `$HOME`, a protected directory, a git work tree or a
    /// directory holding a project manifest (ADR §2).
    pub(crate) fn claimable(&self, path: &Path, depth: u32, has_child: impl Fn(&str) -> bool) -> bool {
        depth > 0 && path != self.home && !self.protected.contains(path) && !has_child(".git")
    }

    /// Matchers that recognize a directory only by a file inside it
    /// (`pyvenv.cfg`, `CMakeCache.txt`, `CACHEDIR.TAG`) never claim a directory
    /// holding a project manifest (`python -m venv .` inside a project). Name
    /// rules are exempt: Next.js writes its own `.next/package.json`.
    pub(crate) fn content_matcher_allowed(matcher: &Matcher, has_child: impl Fn(&str) -> bool) -> bool {
        !matches!(matcher, Matcher::ContainsFile(_) | Matcher::CmakeBuild | Matcher::CacheDirTag)
            || !PROJECT_MANIFESTS.iter().any(|m| has_child(m))
    }

    /// Re-checks on the live filesystem that `path` still matches `rule` —
    /// the same matching code the walk uses (for the executor's re-check).
    pub fn verify(&self, rule: &Rule, path: &Path) -> bool {
        let Some(tag) = self.tag_of(rule) else {
            return false;
        };
        match rule.matcher {
            Matcher::KnownPaths(_) => self.known.get(path) == Some(&tag),
            Matcher::KnownRegion(_) => self.regions.get(path) == Some(&tag),
            Matcher::KnownChildren(_) => path.parent().and_then(|p| self.children_of.get(p)) == Some(&tag),
            Matcher::NoTraverse(_) | Matcher::Synthetic => false,
            _ => match matcher::FsView::open(path) {
                Some(view) => {
                    let has = |n: &str| matcher::DirView::has_child(&view, n);
                    !self.ancestors.contains(path)
                        && self.claimable(path, 1, has)
                        && Self::content_matcher_allowed(&rule.matcher, has)
                        && matcher::matches(&rule.matcher, &view, &self.home)
                }
                None => false,
            },
        }
    }
}
