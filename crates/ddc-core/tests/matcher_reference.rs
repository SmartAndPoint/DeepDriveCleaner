//! The rule matcher against a deliberately simple reference (story "Coverage
//! floor and property tests for the safety invariants", DDC-005). Trees are
//! generated from names the catalog itself uses; the real [`RuleVisitor`],
//! driven through the [`Visitor`] trait as the walk drives it, and the walk
//! itself must decide every directory as [`reference`] does, and
//! [`Catalog::verify`] must accept every rule the walk claims.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use common::TreeBuilder;
use common::fixtures::TAG;
use ddc_core::platform::PLATFORM;
use ddc_core::rules::{Catalog, KEEP_MARKER, KEPT, Matcher, RULES, RuleVisitor, protected_in_home};
use ddc_core::scan::{
    self, DirCtx, DirListing, EntryKind, EntryMeta, OwnerKind, Post, Pre, Progress, Visitor, WalkOptions,
};
use proptest::prelude::*;
use proptest::sample::{Index, select};

/// Files that mark a project: a content matcher never claims a directory
/// holding one (`PROJECT_MANIFESTS` in `rules/mod.rs`).
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

/// Names a CMake build directory conventionally has, and a few near misses.
const CMAKE_NAMES: &[&str] = &["build", "build-release", "cmake-build-debug", "out", "_build", ".build", "outx"];

/// 256 cases unless `PROPTEST_CASES` says otherwise (proptest's default, read
/// from the environment); shrinking stops after 30 s unless
/// `PROPTEST_MAX_SHRINK_TIME` sets it.
fn config() -> ProptestConfig {
    let base = ProptestConfig::default();
    let max_shrink_time = if base.max_shrink_time == 0 { 30_000 } else { base.max_shrink_time };
    ProptestConfig { max_shrink_time, ..base }
}

/// What a file holds, where its content matters to a matcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Content {
    Plain,
    CacheDirTag { valid: bool },
    CmakeCache(CmakeSource),
}

/// `CMAKE_HOME_DIRECTORY` of a generated `CMakeCache.txt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CmakeSource {
    /// The home folder: outside the build directory.
    Outside,
    /// The build directory itself (in-source).
    Itself,
    /// A folder that does not exist.
    Missing,
    /// No such line.
    Unset,
}

/// How many contents a generated file can have: a valid `CACHEDIR.TAG` or a
/// `CMakeCache.txt` naming an outside source half the time.
const VARIANTS: u8 = 6;

impl Content {
    fn of(name: &str, variant: u8) -> Self {
        match name {
            "CACHEDIR.TAG" => Content::CacheDirTag { valid: variant < 3 },
            "CMakeCache.txt" => Content::CmakeCache(match variant {
                0..=2 => CmakeSource::Outside,
                3 => CmakeSource::Itself,
                4 => CmakeSource::Missing,
                _ => CmakeSource::Unset,
            }),
            _ => Content::Plain,
        }
    }
}

/// A generated home folder: directories and files relative to it, and the
/// excludes the scan is given. `""` is the home folder itself.
#[derive(Debug, Clone, Default)]
struct Tree {
    dirs: BTreeSet<PathBuf>,
    files: BTreeMap<PathBuf, Content>,
    excludes: Vec<PathBuf>,
}

/// One generated directory: under a folder the catalog names, a few more
/// components, files in it and files beside it.
type Seed = (String, Vec<String>, Vec<(String, u8)>, Vec<(String, u8)>);

impl Tree {
    fn build(seeds: Vec<Seed>, exclude: Option<Index>) -> Self {
        let mut t = Tree::default();
        for (base, comps, files, beside) in seeds {
            let mut dir = PathBuf::from(base);
            dir.extend(comps);
            t.add_dir(&dir);
            for (name, variant) in files {
                t.add_file(&dir.join(&name), Content::of(&name, variant));
            }
            if let Some(parent) = dir.parent() {
                for (name, variant) in beside {
                    t.add_file(&parent.join(&name), Content::of(&name, variant));
                }
            }
        }
        // A name that is a directory somewhere is never also a file there.
        let dirs = &t.dirs;
        t.files.retain(|path, _| !dirs.contains(path));
        if let Some(i) = exclude {
            let dirs: Vec<&PathBuf> = t.dirs.iter().collect();
            t.excludes.push(i.get(&dirs).to_path_buf());
        }
        t
    }

    fn add_dir(&mut self, dir: &Path) {
        let dir = self.spelled(dir);
        for a in dir.ancestors() {
            self.dirs.insert(a.to_path_buf());
        }
    }

    fn add_file(&mut self, file: &Path, content: Content) {
        let file = self.spelled(file);
        if let Some(parent) = file.parent() {
            self.add_dir(parent);
        }
        self.files.insert(file, content);
    }

    /// `path` with each name spelled as the tree already spells it in any case:
    /// macOS folds `Caches` and `caches` into one entry, so the tree never holds both.
    fn spelled(&self, path: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for comp in path.iter() {
            let want = out.join(comp);
            let same = |p: &&PathBuf| {
                p.parent() == Some(out.as_path()) && p.file_name().is_some_and(|n| n.eq_ignore_ascii_case(comp))
            };
            let have = self.dirs.iter().find(same).or_else(|| self.files.keys().find(same));
            out = have.cloned().unwrap_or(want);
        }
        out
    }

    fn has(&self, dir: &Path, name: &str) -> bool {
        let p = dir.join(name);
        self.dirs.contains(&p) || self.files.contains_key(&p)
    }

    fn children(&self, dir: &Path) -> impl Iterator<Item = &PathBuf> {
        self.dirs.iter().filter(move |d| d.parent() == Some(dir))
    }

    /// The listing the walk would read, in memory.
    fn listing(&self, dir: &Path) -> DirListing {
        let entry = |path: &PathBuf, kind| EntryMeta {
            name: path.file_name().map(OsString::from).unwrap_or_default(),
            kind,
            dev: 0,
            ino: 0,
            nlink: 1,
            uid: None,
            flags: 0,
            alloc: 0,
            private: None,
            error: None,
            modified: None,
            added: None,
        };
        let dirs = self.children(dir).map(|d| entry(d, EntryKind::Dir));
        let files = self.files.keys().filter(|f| f.parent() == Some(dir)).map(|f| entry(f, EntryKind::File));
        DirListing { dev: 0, ino: 0, modified: None, alloc: 0, entries: dirs.chain(files).collect() }
    }

    fn materialize(&self, home: &Path) {
        for dir in &self.dirs {
            fs::create_dir_all(abs(home, dir)).expect("create dir");
        }
        for (file, content) in &self.files {
            let text = match content {
                Content::Plain => String::new(),
                Content::CacheDirTag { valid: true } => TAG.to_owned(),
                Content::CacheDirTag { valid: false } => "Signature: not a cache\n".to_owned(),
                Content::CmakeCache(source) => {
                    let dir = abs(home, file.parent().unwrap_or(Path::new("")));
                    let source = match source {
                        CmakeSource::Outside => home.to_path_buf(),
                        CmakeSource::Itself => dir,
                        CmakeSource::Missing => home.join("no-such-source"),
                        CmakeSource::Unset => PathBuf::new(),
                    };
                    if source.as_os_str().is_empty() {
                        "CMAKE_BUILD_TYPE:STRING=Debug\n".to_owned()
                    } else {
                        format!("CMAKE_HOME_DIRECTORY:INTERNAL={}\n", source.display())
                    }
                }
            };
            fs::write(abs(home, file), text).expect("write file");
        }
    }
}

/// `rel` under `home`; `""` is `home` itself, with no trailing separator.
fn abs(home: &Path, rel: &Path) -> PathBuf {
    if rel.as_os_str().is_empty() { home.to_path_buf() } else { home.join(rel) }
}

/// What happens to one directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Verdict {
    /// Not opened.
    Skip,
    /// A finding of this rule; nothing inside is matched.
    Claim(&'static str),
    /// A region of this rule; matching goes on inside.
    Region(&'static str),
    /// Nothing; matching goes on inside.
    Descend,
}

/// The reference matcher: a linear scan over [`RULES`], in this order, the
/// first hit wins (ADR "Cleanup safety model: risk classes, plan and
/// confirmation", §2 and §6; code in `rules/visitor.rs` and `rules/mod.rs`).
///
/// Before the directory is read (`RuleVisitor::before_read`, visitor.rs:121-149):
/// 1. a cloud folder (`NoTraverse`) is not opened (visitor.rs:123);
/// 2. at or under an exclude: `user.kept` (visitor.rs:135);
/// 3. an exact known path (visitor.rs:138), then a region (visitor.rs:141),
///    then a child of a known parent (visitor.rs:145), each in catalog order;
///    a `.ddc-keep` in the directory turns the claim into `user.kept`, and a
///    region with one becomes a `user.kept` claim.
///
/// After it is read (`RuleVisitor::after_read`, visitor.rs:151-170), also
/// for a region's own directory:
/// 4. a `.ddc-keep` in it: `user.kept` (visitor.rs:154);
/// 5. never claimed: an ancestor of a known path or a known parent
///    (visitor.rs:157), the home folder, a protected folder or a directory
///    holding `.git` (`Catalog::claimable`, mod.rs:288-289);
/// 6. the specialized rules in catalog order, where a rule that looks only at
///    a file inside (`pyvenv.cfg`, `CMakeCache.txt`, `CACHEDIR.TAG`) skips a
///    directory holding a project manifest (`Catalog::content_matcher_allowed`,
///    mod.rs:296; visitor.rs:163);
/// 7. last, the generic `CACHEDIR.TAG` rule (mod.rs:236, visitor.rs:163).
///
/// Entry rules (`ENTRY_RULES`) play no part here: they decide only whether a
/// claim lists its children.
fn reference(t: &Tree, rel: &Path) -> Verdict {
    let kept_or = |id| if t.has(rel, KEEP_MARKER) { KEPT } else { id };
    let named = |paths: &[&str]| paths.iter().any(|p| rel == Path::new(p));

    // Before the read.
    if RULES.iter().any(|r| matches!(r.matcher, Matcher::NoTraverse(paths) if named(paths))) {
        return Verdict::Skip;
    }
    if t.excludes.iter().any(|e| rel.starts_with(e)) {
        return Verdict::Claim(KEPT);
    }
    if let Some(r) = RULES.iter().find(|r| matches!(r.matcher, Matcher::KnownPaths(paths) if named(paths))) {
        return Verdict::Claim(kept_or(r.id));
    }
    let mut region = None;
    if let Some(r) = RULES.iter().find(|r| matches!(r.matcher, Matcher::KnownRegion(paths) if named(paths))) {
        match kept_or(r.id) {
            KEPT => return Verdict::Claim(KEPT),
            id => region = Some(id),
        }
    }
    if region.is_none()
        && let Some(r) = RULES
            .iter()
            .find(|r| matches!(r.matcher, Matcher::KnownChildren(parents) if parents.iter().any(|p| rel.parent() == Some(Path::new(p)))))
    {
        return Verdict::Claim(kept_or(r.id));
    }
    let not_claimed = region.map_or(Verdict::Descend, Verdict::Region);

    // After the read.
    if t.has(rel, KEEP_MARKER) {
        return Verdict::Claim(KEPT);
    }
    if holds_known_path(rel) || rel.as_os_str().is_empty() || named(protected_in_home(PLATFORM)) || t.has(rel, ".git") {
        return not_claimed;
    }
    let manifest = PROJECT_MANIFESTS.iter().any(|m| t.has(rel, m));
    let specialized = RULES.iter().filter(|r| !matches!(r.matcher, Matcher::CacheDirTag));
    let generic = RULES.iter().filter(|r| matches!(r.matcher, Matcher::CacheDirTag));
    for r in specialized.chain(generic) {
        let content = matches!(r.matcher, Matcher::ContainsFile(_) | Matcher::CmakeBuild | Matcher::CacheDirTag);
        if !(content && manifest) && reference_matches(t, rel, &r.matcher) {
            return Verdict::Claim(r.id);
        }
    }
    not_claimed
}

/// `rel` lies strictly above a known path or region, or is a known parent.
fn holds_known_path(rel: &Path) -> bool {
    let above = |p: &str| Path::new(p).starts_with(rel) && Path::new(p) != rel;
    !rel.as_os_str().is_empty()
        && RULES.iter().any(|r| match r.matcher {
            Matcher::KnownPaths(paths) | Matcher::KnownRegion(paths) => paths.iter().any(|p| above(p)),
            Matcher::KnownChildren(parents) => parents.iter().any(|p| above(p) || rel == Path::new(p)),
            _ => false,
        })
}

/// One specialized matcher against the generated tree.
fn reference_matches(t: &Tree, rel: &Path, m: &Matcher) -> bool {
    let name = rel.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let parent = rel.parent().unwrap_or(Path::new(""));
    match *m {
        Matcher::Name { names, siblings, children_any } => {
            names.contains(&name)
                && (siblings.is_empty() || siblings.iter().any(|s| t.has(parent, s)))
                && (children_any.is_empty() || children_any.iter().any(|c| t.dirs.contains(&rel.join(c))))
        }
        Matcher::ContainsFile(file) => t.has(rel, file),
        Matcher::NameSuffix(suffix) => name.len() > suffix.len() && name.ends_with(suffix),
        Matcher::NameContains(text) => name.contains(text),
        Matcher::CmakeBuild => {
            let conventional = name.starts_with("build")
                || name.starts_with("cmake-build-")
                || matches!(name, "out" | "_build" | ".build");
            conventional
                && !t.has(rel, "CMakeLists.txt")
                && t.files.get(&rel.join("CMakeCache.txt")) == Some(&Content::CmakeCache(CmakeSource::Outside))
        }
        Matcher::CacheDirTag => t.files.get(&rel.join("CACHEDIR.TAG")) == Some(&Content::CacheDirTag { valid: true }),
        Matcher::Under { name: want, prefixes } => name == want && prefixes.iter().any(|p| rel.starts_with(p)),
        Matcher::ChildHolding { parents, file } => {
            parents.iter().any(|p| parent == Path::new(p)) && t.files.contains_key(&rel.join(file))
        }
        Matcher::KnownPaths(_)
        | Matcher::KnownRegion(_)
        | Matcher::KnownChildren(_)
        | Matcher::NoTraverse(_)
        | Matcher::Synthetic => false,
    }
}

/// What the real visitor decides for `rel`, its hooks called as the walk
/// calls them: `before_read`, then `after_read` unless that claimed or skipped.
fn visitor_verdict(visitor: &dyn Visitor, catalog: &Catalog, home: &Path, t: &Tree, rel: &Path) -> Verdict {
    let path = abs(home, rel);
    let depth = rel.components().count() as u32;
    let ctx = DirCtx { path: &path, name: path.file_name().unwrap_or_default(), depth, dev: 0, ino: 0, owner: None };
    let region = match visitor.before_read(&ctx) {
        Pre::Skip(_) => return Verdict::Skip,
        Pre::Claim(tag) => return Verdict::Claim(catalog.rule(tag).id),
        Pre::Region(tag) => Some(tag),
        Pre::Continue => None,
    };
    let ctx = DirCtx { owner: region.map(|tag| (OwnerKind::Region, tag)), ..ctx };
    match (visitor.after_read(&ctx, &t.listing(rel)), region) {
        (Post::Claim(tag), _) => Verdict::Claim(catalog.rule(tag).id),
        (Post::Descend, Some(tag)) => Verdict::Region(catalog.rule(tag).id),
        (Post::Descend, None) => Verdict::Descend,
    }
}

/// The reference applied top-down from the home folder, as the walk goes:
/// a claim hides its subtree, a skipped folder is not opened.
fn reference_walk(t: &Tree) -> BTreeMap<PathBuf, Verdict> {
    let mut found = BTreeMap::new();
    let mut stack = vec![PathBuf::new()];
    while let Some(rel) = stack.pop() {
        let verdict = reference(t, &rel);
        match verdict {
            Verdict::Skip => continue,
            Verdict::Claim(_) => {
                found.insert(rel, verdict);
                continue;
            }
            Verdict::Region(_) => {
                found.insert(rel.clone(), verdict);
            }
            Verdict::Descend => {}
        }
        stack.extend(t.children(&rel).cloned());
    }
    found
}

/// Every directory the walk made a finding or a region, by path; a claim of
/// a region's own directory wins over the region.
fn walk_found(visitor: &RuleVisitor<'_>, catalog: &Catalog, home: &Path) -> BTreeMap<PathBuf, Verdict> {
    let opts = WalkOptions { threads: Some(2), ..Default::default() };
    let out = scan::walk(&[home.to_path_buf()], visitor, &opts, &Progress::default()).expect("walk");
    let mut found = BTreeMap::new();
    for owner in out.owners {
        let rel = owner.path.strip_prefix(home).expect("owner inside home").to_path_buf();
        let id = catalog.rule(owner.tag).id;
        let verdict = match owner.kind {
            OwnerKind::Claim => Verdict::Claim(id),
            OwnerKind::Region => Verdict::Region(id),
        };
        let slot = found.entry(rel).or_insert(verdict);
        if matches!(verdict, Verdict::Claim(_)) {
            *slot = verdict;
        }
    }
    found
}

/// Names the catalog uses, for the generator.
struct Vocab {
    /// Home-relative folders the catalog names: known paths, regions, known
    /// parents, prefixes, cloud and protected folders, and their parents.
    bases: Vec<String>,
    /// Directory names: components of those folders and the names rules match.
    dir_names: Vec<String>,
    /// File names the matchers look for, beside or inside a directory.
    file_names: Vec<String>,
    /// Files that alone make a directory a finding (`pyvenv.cfg`, `CACHEDIR.TAG`).
    content_files: Vec<String>,
    /// One per specialized rule: what a directory it matches looks like.
    artifacts: Vec<Artifact>,
}

/// The parts of a directory one specialized rule looks for, so a generated
/// directory often has them together; noise and other markers mix in.
#[derive(Debug, Clone)]
struct Artifact {
    names: Vec<String>,
    /// Files beside the directory.
    beside: Vec<String>,
    /// Files inside it, maybe one level down (`classes/f`, `conda-meta/history`).
    inside: Vec<String>,
    /// Home-relative folders the rule matches only under (`Under`, `ChildHolding`).
    homes: Vec<String>,
}

impl Vocab {
    fn from_catalog() -> Self {
        let mut bases = BTreeSet::new();
        let mut dir_names = BTreeSet::new();
        let mut file_names = BTreeSet::new();
        let mut content_files = Vec::new();
        let mut artifacts = Vec::new();
        let mut base = |p: &str, dir_names: &mut BTreeSet<String>| {
            for a in Path::new(p).ancestors().filter(|a| !a.as_os_str().is_empty()) {
                bases.insert(a.to_string_lossy().into_owned());
            }
            dir_names.extend(p.split('/').filter(|c| !c.is_empty()).map(str::to_owned));
        };
        let own = |names: &[&str]| names.iter().map(|n| (*n).to_owned()).collect::<Vec<_>>();
        let mut artifact = |names: Vec<String>, beside: Vec<String>, inside: Vec<String>, homes: Vec<String>| {
            artifacts.push(Artifact { names, beside, inside, homes });
        };
        for p in protected_in_home(PLATFORM) {
            base(p, &mut dir_names);
        }
        for rule in RULES {
            match rule.matcher {
                Matcher::KnownPaths(paths) | Matcher::KnownRegion(paths) | Matcher::NoTraverse(paths) => {
                    paths.iter().for_each(|p| base(p, &mut dir_names));
                }
                Matcher::KnownChildren(parents) => parents.iter().for_each(|p| base(p, &mut dir_names)),
                Matcher::Name { names, siblings, children_any } => {
                    dir_names.extend(own(names));
                    dir_names.extend(own(children_any));
                    file_names.extend(own(siblings));
                    file_names.extend(own(children_any));
                    let inside = children_any.iter().flat_map(|c| [format!("{c}/f"), (*c).to_owned()]).collect();
                    artifact(own(names), own(siblings), inside, vec![]);
                }
                Matcher::ContainsFile(file) => {
                    file_names.insert(file.to_owned());
                    content_files.push(file.to_owned());
                    artifact(own(&[".venv", "env"]), own(PROJECT_MANIFESTS), own(&[file]), vec![]);
                }
                Matcher::NameSuffix(suffix) => {
                    let names = vec![format!("x{suffix}"), suffix.to_owned()];
                    dir_names.extend(names.clone());
                    artifact(names, vec![], vec![], vec![]);
                }
                Matcher::NameContains(text) => {
                    let names = vec![format!("x{text}1"), text.trim_end_matches('-').to_owned()];
                    dir_names.extend(names.clone());
                    artifact(names, vec![], vec![], vec![]);
                }
                Matcher::CmakeBuild => {
                    dir_names.extend(own(CMAKE_NAMES));
                    file_names.extend(own(&["CMakeCache.txt", "CMakeLists.txt"]));
                    content_files.push("CMakeCache.txt".to_owned());
                    artifact(
                        own(CMAKE_NAMES),
                        own(&["CMakeLists.txt"]),
                        own(&["CMakeCache.txt", "CMakeLists.txt"]),
                        vec![],
                    );
                }
                Matcher::CacheDirTag => {
                    file_names.insert("CACHEDIR.TAG".to_owned());
                    content_files.push("CACHEDIR.TAG".to_owned());
                    artifact(own(&["somecache", "build"]), vec![], own(&["CACHEDIR.TAG"]), vec![]);
                }
                Matcher::Under { name, prefixes } => {
                    dir_names.insert(name.to_owned());
                    prefixes.iter().for_each(|p| base(p, &mut dir_names));
                    artifact(own(&[name]), vec![], vec![], own(prefixes));
                }
                Matcher::ChildHolding { parents, file } => {
                    parents.iter().for_each(|p| base(p, &mut dir_names));
                    file_names.insert(file.to_owned());
                    artifact(
                        own(&["old", "base"]),
                        vec![],
                        own(&[file, file.split('/').next().unwrap_or(file)]),
                        own(parents),
                    );
                }
                Matcher::Synthetic => {}
            }
            for lock in rule.lockfiles.unwrap_or_default().iter().filter(|l| !l.contains('*')) {
                file_names.insert((*lock).to_owned());
            }
        }
        file_names.extend(own(PROJECT_MANIFESTS));
        file_names.extend(own(&[KEEP_MARKER, ".git"]));
        dir_names.insert(".git".to_owned());
        Self {
            bases: bases.into_iter().collect(),
            dir_names: dir_names.into_iter().collect(),
            file_names: file_names.into_iter().collect(),
            content_files,
            artifacts,
        }
    }
}

/// Small home folders built from the catalog's own names and some noise:
/// folders the catalog names with names under them; directories shaped like
/// one specialized rule's match, in a project or a catalog folder; and
/// catalog folders, protected ones most of all, holding a file that alone
/// makes a finding.
fn trees() -> impl Strategy<Value = Tree> {
    let v = Vocab::from_catalog();
    let protected: Vec<String> =
        protected_in_home(PLATFORM).iter().filter(|p| !p.is_empty()).map(|p| (*p).to_owned()).collect();
    let folder = (
        prop_oneof![select(protected), select(v.bases.clone())],
        prop::collection::vec((select(v.content_files), 0..VARIANTS), 1..=2),
    )
        .prop_map(|(base, markers)| -> Seed { (base, vec![], markers, vec![]) });
    let base = prop_oneof![1 => Just(String::new()), 4 => select(v.bases.clone())];
    let component = prop_oneof![4 => select(v.dir_names), 1 => "[a-z]{1,3}"];
    let file = (prop_oneof![4 => select(v.file_names), 1 => "[a-z]{1,3}"], 0..VARIANTS);
    let plain = (
        base,
        prop::collection::vec(component, 0..=2),
        prop::collection::vec(file.clone(), 0..=3),
        prop::collection::vec(file.clone(), 0..=2),
    );
    let artifacts = v.artifacts;
    let artifact = (
        prop_oneof![2 => Just(String::new()), 1 => select(v.bases)],
        prop::option::of("[a-z]{1,3}"),
        any::<(Index, Index)>(),
        any::<(bool, Index)>(),
        prop::collection::vec((any::<Index>(), 0..VARIANTS), 0..=2),
        prop::collection::vec((any::<Index>(), 0..VARIANTS), 0..=2),
        prop::collection::vec(file, 0..=1),
    )
        .prop_map(move |(base, project, (which, name), (at_home, home), beside, inside, extra)| -> Seed {
            let a = which.get(&artifacts);
            let (base, project) =
                if at_home && !a.homes.is_empty() { (home.get(&a.homes).clone(), None) } else { (base, project) };
            let pick = |from: &[String], picks: Vec<(Index, u8)>| -> Vec<(String, u8)> {
                if from.is_empty() {
                    vec![]
                } else {
                    picks.into_iter().map(|(i, v)| (i.get(from).clone(), v)).collect()
                }
            };
            let comps = project.into_iter().chain([name.get(&a.names).clone()]).collect();
            (base, comps, pick(&a.inside, inside).into_iter().chain(extra).collect(), pick(&a.beside, beside))
        });
    let seed = prop_oneof![2 => plain, 2 => artifact, 1 => folder];
    (prop::collection::vec(seed, 1..=8), prop::option::weighted(0.15, any::<Index>()))
        .prop_map(|(seeds, exclude)| Tree::build(seeds, exclude))
}

/// The generated tree on disk, as a home folder in a temporary directory,
/// with its excludes made absolute.
fn on_disk(t: &Tree) -> (TreeBuilder, Vec<PathBuf>) {
    let tb = TreeBuilder::new();
    t.materialize(tb.root());
    let excludes = t.excludes.iter().map(|e| abs(tb.root(), e)).collect();
    (tb, excludes)
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn every_directory_gets_the_reference_verdict_from_the_rule_visitor(t in trees()) {
        let (tb, excludes) = on_disk(&t);
        let home = tb.root();
        let catalog = Catalog::builtin(home);
        let visitor = RuleVisitor::new(&catalog, excludes);
        for rel in &t.dirs {
            let real = visitor_verdict(&visitor, &catalog, home, &t, rel);
            prop_assert_eq!(real, reference(&t, rel), "directory {:?}", rel);
            if let Verdict::Claim(id) | Verdict::Region(id) = real
                && id != KEPT
            {
                let rule = catalog.by_id(id).expect("rule in catalog");
                prop_assert!(catalog.verify(rule, &abs(home, rel)), "verify refuses {} at {:?}", id, rel);
            }
        }
    }

    #[test]
    fn the_walk_claims_what_the_reference_claims_and_verify_agrees(t in trees()) {
        if common::unsupported("the_walk_claims_what_the_reference_claims_and_verify_agrees") {
            return Ok(());
        }
        let (tb, excludes) = on_disk(&t);
        let home = tb.root();
        let catalog = Catalog::builtin(home);
        let visitor = RuleVisitor::new(&catalog, excludes);
        let found = walk_found(&visitor, &catalog, home);
        prop_assert_eq!(&found, &reference_walk(&t));
        for (rel, verdict) in &found {
            if let Verdict::Claim(id) | Verdict::Region(id) = *verdict
                && id != KEPT
            {
                let rule = catalog.by_id(id).expect("rule in catalog");
                prop_assert!(catalog.verify(rule, &abs(home, rel)), "verify refuses {} at {:?}", id, rel);
            }
        }
    }
}

/// The case CI's macOS runner found: two names that differ only in case are one
/// folder on disk, so the tree holds one of them and the walk agrees with it.
/// It walks, so it skips where the platform is not supported.
#[test]
fn names_that_differ_only_in_case_are_one_folder() {
    if common::unsupported("names_that_differ_only_in_case_are_one_folder") {
        return;
    }
    let seed = |name: &str| -> Seed { (".gradle".to_owned(), vec![name.to_owned()], vec![], vec![]) };
    let t = Tree::build(vec![seed("caches"), seed("Caches")], None);
    assert!(!t.dirs.contains(Path::new(".gradle/Caches")), "{:?}", t.dirs);
    let (tb, excludes) = on_disk(&t);
    let catalog = Catalog::builtin(tb.root());
    let visitor = RuleVisitor::new(&catalog, excludes);
    assert_eq!(walk_found(&visitor, &catalog, tb.root()), reference_walk(&t));
}
