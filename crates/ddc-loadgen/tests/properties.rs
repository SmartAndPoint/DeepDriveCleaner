//! The safety invariants as properties over generated trees (story
//! "Coverage floor and property tests for the safety invariants", DDC-005).
//!
//! A case is a list of placements built with [`ddc_loadgen::desc`] and the
//! rule fixtures of [`ddc_loadgen::fixtures`], so a new rule joins the
//! strategy by itself and shrinking drops whole placements. The tree is
//! built in a marked temporary folder, and the scan root is the tree itself,
//! which is also the home: the protected folders lie inside the root. The
//! scan runs in process with `ReaderKind::Std`, a fixed `now` an hour after
//! the build and a fixed volume; the executor runs with a Trash folder in the
//! scratch, no tools and an empty process table. Nothing here names
//! an OS module of `ddc_core::platform`: the tests reach the core through
//! seams that survive epic DDC-007's move of the OS calls.
//!
//! Which placements feed which property:
//! - every property: a rule fixture at a depth under a plain, hidden or
//!   `Library` folder, or at the home itself, with or without a lock file,
//!   `.git` and recent activity; a home-anchored rule's fixture; a VM rule's
//!   path with a `CACHEDIR.TAG` inside it, at its top or in its parent; a rule
//!   fixture under a cloud folder; a `.ddc-keep` at a finding's top (it
//!   becomes `user.kept`); a symlink to a protected folder or out of the tree;
//!   hard links within one finding, across two, and from outside a finding.
//! - all but counted once, since the manifest does not model them: a
//!   `.ddc-keep` deep inside a finding or in a folder above it; a finding
//!   nested in another; a `CACHEDIR.TAG` or `pyvenv.cfg` directly in a
//!   protected folder the walk asks `Catalog::claimable` about (drawn in at
//!   least one case in ten); `.git` or a manifest at a would-be finding's
//!   top; a `.ddc-keep` in an entry of the user's own folders.
//!
//! Not covered here: that `--yes` runs only the default-selected findings of
//! its classes (`exec::allowed`, tested in `ddc-core/tests/clean_safety.rs`).

// Unix only: the placements are byte paths, and the checks read st_dev and st_ino.
#![cfg(unix)]

use std::cell::Cell;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};

use ddc_core::class::{Category, Removal, RiskClass};
use ddc_core::env::Env;
use ddc_core::exec::{
    self, CmdOutput, CommandRunner, Consent, ExecOptions, MemoryJournal, OpenVnode, Outcome, Ports, ProcInfo,
    ProcStart, ProcessTable, SkipReason, Trasher,
};
use ddc_core::model::{Finding, RemovalKind, ScanReport};
use ddc_core::plan::{
    Action, ActionKind, ListedItem, Plan, PlanItem, PlanOptions, action_for, build_plan, plan_listed,
};
use ddc_core::platform::PLATFORM;
use ddc_core::report::contents::{EntryIndex, EntryState};
use ddc_core::report::volumes::{StatFs, VolumeStats};
use ddc_core::report::{self, ScanOptions};
use ddc_core::rules::entries::{ENTRY_RULES, EntryLock};
use ddc_core::rules::{
    Catalog, KEEP_MARKER, KEPT, Matcher, RULES, Rule, RuleVisitor, protected_in_home, protected_system,
};
use ddc_core::scan::{self, DirListing, DirReader, Progress, ReadError, ReaderKind, StdReader, Times, WalkOptions};
use ddc_loadgen::desc::{BLOCK, Desc, join, parent};
use ddc_loadgen::fixtures::{self, TAG};
use ddc_loadgen::manifest::manifest;
use ddc_loadgen::rng::Rng;
use ddc_loadgen::target::{self, Layout, Scratch};
use ddc_loadgen::{build, verify};
use proptest::option;
use proptest::prelude::*;
use proptest::sample::{Index, select};

/// 256 cases unless `PROPTEST_CASES` says otherwise (proptest's default, read
/// from the environment; `make gate-fast` alone sets 32); shrinking stops
/// after 30 s unless `PROPTEST_MAX_SHRINK_TIME` sets it, well before the
/// `load` group's 240 s kill.
fn config() -> ProptestConfig {
    let base = ProptestConfig::default();
    let max_shrink_time = if base.max_shrink_time == 0 { 30_000 } else { base.max_shrink_time };
    ProptestConfig { max_shrink_time, ..base }
}

/// The protected folders of this platform: relative to the home, and absolute.
fn protected() -> (&'static [&'static str], &'static [&'static str]) {
    (protected_in_home(PLATFORM), protected_system(PLATFORM))
}

/// Protected folders the walk asks `Catalog::claimable` about. The others
/// are claimed or descended before the question (`rules/visitor.rs`
/// `before_read`, `after_read`), so a marker in them tests nothing.
const CLAIM_CHECKED: &[&str] = &["Documents", "Desktop", "Projects", ".ssh", ".gnupg", ".config"];

/// What a scan of the home must find under a project rule's fixture.
static PROJECT: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| fixtures::project_rules().iter().map(|r| r.id).collect());

static HOME: LazyLock<Vec<&'static str>> = LazyLock::new(|| fixtures::home_rules().iter().map(|r| r.id).collect());

/// Rules for VM runtimes, with the paths they claim.
static VM: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    RULES
        .iter()
        .filter(|r| r.category == Category::VmContainer && matches!(r.matcher, Matcher::KnownPaths(_)))
        .map(|r| r.id)
        .collect()
});

/// Rules whose only removal is their runtime's own commands.
static NATIVE_ONLY: LazyLock<Vec<&'static str>> =
    LazyLock::new(|| RULES.iter().filter(|r| matches!(r.removal, Removal::NativeOnly(_))).map(|r| r.id).collect());

/// Folders the walk never opens (`Library/CloudStorage`, `Library/Mobile Documents`).
static CLOUD: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    RULES
        .iter()
        .filter_map(|r| match r.matcher {
            Matcher::NoTraverse(paths) => Some(paths),
            _ => None,
        })
        .flatten()
        .copied()
        .collect()
});

/// The user's own folders this platform lists, whose entries a list may name.
static ENTRY_FOLDERS: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    ENTRY_RULES.iter().filter(|id| RULES.iter().any(|r| r.id == **id)).map(|id| anchored(rule(id))[0]).collect()
});

/// First components of the paths a home rule anchors and of the protected
/// folders: a fixture at the home itself must not take one.
static RESERVED: LazyLock<HashSet<&'static [u8]>> = LazyLock::new(|| {
    let mut paths: Vec<&'static str> = protected().0.to_vec();
    for r in RULES {
        paths.extend(anchored(r));
        if let Matcher::Under { prefixes, .. } = r.matcher {
            paths.extend(prefixes);
        }
    }
    paths.iter().filter(|p| !p.is_empty()).map(|p| first(p.as_bytes())).collect()
});

fn rule(id: &str) -> &'static Rule {
    RULES.iter().find(|r| r.id == id).unwrap_or_else(|| panic!("no rule {id}"))
}

/// The home-relative paths a rule is anchored at.
fn anchored(r: &'static Rule) -> Vec<&'static str> {
    match r.matcher {
        Matcher::KnownPaths(p) | Matcher::KnownRegion(p) | Matcher::NoTraverse(p) => p.to_vec(),
        Matcher::KnownChildren(p) => p.to_vec(),
        Matcher::ChildHolding { parents, .. } => parents.to_vec(),
        _ => Vec::new(),
    }
}

fn first(path: &[u8]) -> &[u8] {
    path.split(|&c| c == b'/').next().unwrap_or_default()
}

/// The rule that claims a folder holding a valid `CACHEDIR.TAG`.
fn cachedir_rule() -> &'static Rule {
    RULES.iter().find(|r| r.matcher == Matcher::CacheDirTag).expect("a CACHEDIR.TAG rule")
}

// ---- the strategy ----------------------------------------------------------

/// A project inside a cloud folder; where the catalog has none (Linux), a
/// kept project in its place.
fn cloud() -> BoxedStrategy<Part> {
    if CLOUD.is_empty() {
        return project_rule().prop_map(|rule| Part::KeptTop { rule }).boxed();
    }
    (select(CLOUD.clone()), project_rule(), any::<bool>())
        .prop_map(|(folder, rule, lock)| Part::Cloud { folder, rule, lock })
        .boxed()
}

/// Where a project rule's fixture goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Base {
    /// `w<i>/…`
    Plain,
    /// `.w<i>/…`: a top-level hidden folder of the home, so app-owned.
    Hidden,
    /// `Library/w<i>/…`: app-owned.
    Library,
    /// The home itself, once per tree, where its names are free.
    Home,
}

/// Where a VM rule's path gets a `CACHEDIR.TAG`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spot {
    Inside,
    Top,
    Parent,
}

/// Where a symlink points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// A protected folder of the home, by a relative path.
    Protected(&'static str),
    /// A folder beside the tree, by its absolute path.
    Outside,
    Root,
}

#[derive(Debug, Clone)]
enum Part {
    Project { rule: &'static str, base: Base, depth: u8, lock: bool, git: bool, fresh: bool, files: u8 },
    Home { rule: &'static str, seed: u64 },
    Vm { rule: &'static str, marker: Spot },
    Cloud { folder: &'static str, rule: &'static str, lock: bool },
    KeptTop { rule: &'static str },
    Symlink { inside: Option<&'static str>, to: Target },
    HardLinks { a: &'static str, b: &'static str, across: bool, outside: bool },
    // Not modeled by the manifest: the safety properties only.
    KeepDeep { rule: &'static str, depth: u8 },
    KeepAbove { rule: &'static str, depth: u8 },
    Nested { outer: &'static str, inner: &'static str },
    InProtected { folder: &'static str, venv: bool },
    AtTop { rule: &'static str, git: bool },
    EntryKept { folder: &'static str, top: bool },
}

fn project_rule() -> impl Strategy<Value = &'static str> + Clone {
    select(PROJECT.clone())
}

fn in_protected() -> impl Strategy<Value = Part> {
    (select(CLAIM_CHECKED), any::<bool>()).prop_map(|(folder, venv)| Part::InProtected { folder, venv })
}

/// The placements the manifest models; with `all`, the others too.
fn part(all: bool) -> BoxedStrategy<Part> {
    let base =
        prop_oneof![4 => Just(Base::Plain), 1 => Just(Base::Hidden), 1 => Just(Base::Library), 1 => Just(Base::Home)];
    let spot = prop_oneof![Just(Spot::Inside), Just(Spot::Top), Just(Spot::Parent)];
    let protected_names: Vec<&'static str> = protected().0.iter().copied().filter(|p| !p.is_empty()).collect();
    let target = prop_oneof![
        2 => select(protected_names).prop_map(Target::Protected),
        1 => Just(Target::Outside),
        1 => Just(Target::Root),
    ];
    let modeled = prop_oneof![
        5 => (project_rule(), base, 0u8..3, any::<bool>(), any::<bool>(), any::<bool>(), 0u8..6).prop_map(
            |(rule, base, depth, lock, git, fresh, files)| Part::Project { rule, base, depth, lock, git, fresh, files }
        ),
        2 => (select(HOME.clone()), any::<u64>()).prop_map(|(rule, seed)| Part::Home { rule, seed }),
        1 => (select(VM.clone()), spot).prop_map(|(rule, marker)| Part::Vm { rule, marker }),
        1 => cloud(),
        1 => project_rule().prop_map(|rule| Part::KeptTop { rule }),
        1 => (option::of(project_rule()), target).prop_map(|(inside, to)| Part::Symlink { inside, to }),
        1 => (project_rule(), project_rule(), any::<bool>(), any::<bool>())
            .prop_map(|(a, b, across, outside)| Part::HardLinks { a, b, across, outside }),
    ];
    if !all {
        return modeled.boxed();
    }
    prop_oneof![
        12 => modeled,
        1 => (project_rule(), 1u8..4).prop_map(|(rule, depth)| Part::KeepDeep { rule, depth }),
        1 => (project_rule(), 0u8..3).prop_map(|(rule, depth)| Part::KeepAbove { rule, depth }),
        1 => (project_rule(), project_rule()).prop_map(|(outer, inner)| Part::Nested { outer, inner }),
        1 => in_protected(),
        1 => (project_rule(), any::<bool>()).prop_map(|(rule, git)| Part::AtTop { rule, git }),
        1 => (select(ENTRY_FOLDERS.clone()), any::<bool>()).prop_map(|(folder, top)| Part::EntryKept { folder, top }),
    ]
    .boxed()
}

/// A tree of every kind of placement, with a marker in a claim-checked
/// protected folder in at least one case in five.
fn any_tree() -> impl Strategy<Value = Vec<Part>> {
    (prop::collection::vec(part(true), 0..6), option::weighted(0.2, in_protected())).prop_map(|(mut parts, marker)| {
        parts.extend(marker);
        parts
    })
}

/// A tree the manifest models in full.
fn modeled_tree() -> impl Strategy<Value = Vec<Part>> {
    prop::collection::vec(part(false), 0..6)
}

// ---- building ----------------------------------------------------------------

/// Places parts into one [`Desc`]: each in its own `w<i>` folder, the home
/// fixtures once per rule, a file named twice only once.
struct Builder<'a> {
    d: Desc,
    files: HashSet<Vec<u8>>,
    homes: HashSet<&'static str>,
    cloud: HashSet<&'static str>,
    at_home: bool,
    outside: &'a [u8],
}

impl Builder<'_> {
    fn text_once(&mut self, path: Vec<u8>, text: &str) {
        if self.files.insert(path.clone()) {
            self.d.text(path, text);
        }
    }

    fn home_rule(&mut self, id: &'static str, seed: u64) {
        if self.homes.insert(id) {
            fixtures::home(&mut self.d, b"", rule(id), &mut Rng::new(seed));
        }
    }

    /// A project fixture in `base`, with a few files in it; its path.
    fn project(&mut self, base: &[u8], id: &'static str, lock: bool, rng: &mut Rng, files: u8) -> Vec<u8> {
        let at = fixtures::project(&mut self.d, base, rule(id), lock);
        fixtures::payload(&mut self.d, rng, &at, u64::from(files));
        at
    }

    /// Whether `id`'s fixture can go at the home itself: none of its names
    /// is anchored or protected there.
    fn fits_at_home(id: &'static str) -> bool {
        let mut probe = Desc::new();
        fixtures::project(&mut probe, b"", rule(id), true);
        probe.nodes.iter().all(|n| !RESERVED.contains(first(&n.path)))
    }

    fn place(&mut self, i: usize, part: &Part) {
        let w = format!("w{i}").into_bytes();
        let mut rng = Rng::new(i as u64);
        match *part {
            Part::Project { rule: id, base, depth, lock, git, fresh, files } => {
                let home = base == Base::Home && !self.at_home && Self::fits_at_home(id);
                let top = match base {
                    _ if home => Vec::new(),
                    Base::Hidden => format!(".w{i}").into_bytes(),
                    Base::Library => format!("Library/w{i}").into_bytes(),
                    Base::Plain | Base::Home => w,
                };
                self.at_home |= home;
                let mut dir = top.clone();
                for k in 0..if home { 0 } else { depth } {
                    dir = join(&dir, format!("n{k}").as_bytes());
                }
                let git = git && !home;
                if git {
                    self.d.file(join(&top, b".git/HEAD"), 0);
                    self.d.file(join(&top, b".git/index"), 0);
                }
                let at = self.project(&dir, id, lock, &mut rng, files);
                let lockfile =
                    rule(id).lockfiles.filter(|_| lock).map(|l| join(&dir, l[0].replace('*', "").as_bytes()));
                match (fresh, git, lockfile) {
                    (true, true, _) => self.d.fresh(join(&top, b".git/index")),
                    (true, false, Some(lockfile)) => self.d.fresh(lockfile),
                    _ => {}
                }
                self.d.plant(at, rule(id).id);
            }
            Part::Home { rule, seed } => self.home_rule(rule, seed),
            Part::Vm { rule: id, marker } => {
                self.home_rule(id, i as u64);
                let path = anchored(rule(id))[0].as_bytes();
                let at = match marker {
                    Spot::Inside => join(path, b"m/CACHEDIR.TAG"),
                    Spot::Top => join(path, b"CACHEDIR.TAG"),
                    Spot::Parent => join(parent(path).unwrap_or_default(), b"CACHEDIR.TAG"),
                };
                self.text_once(at, TAG);
            }
            Part::Cloud { folder, rule, lock } => {
                self.project(&join(folder.as_bytes(), &w), rule, lock, &mut rng, 3);
                if self.cloud.insert(folder) {
                    self.d.unopened.push(folder.as_bytes().to_vec());
                }
            }
            Part::KeptTop { rule } => {
                let at = self.project(&w, rule, true, &mut rng, 3);
                self.d.file(join(&at, KEEP_MARKER.as_bytes()), 0);
                self.d.plant(at, KEPT);
            }
            Part::Symlink { inside, to } => {
                let dir = match inside {
                    Some(id) => {
                        let at = self.project(&w, id, true, &mut rng, 2);
                        self.d.plant(&at, rule(id).id);
                        at
                    }
                    None => w,
                };
                let target = match to {
                    Target::Protected(rel) => {
                        let up = dir.split(|&c| c == b'/').count();
                        format!("{}{rel}", "../".repeat(up)).into_bytes()
                    }
                    Target::Outside => self.outside.to_vec(),
                    Target::Root => b"/".to_vec(),
                };
                self.d.symlink(join(&dir, b"l"), target);
            }
            Part::HardLinks { a, b, across, outside } => {
                let at_a = self.project(&join(&w, b"a"), a, true, &mut rng, 0);
                self.d.plant(&at_a, rule(a).id);
                let file = join(&at_a, b"h");
                self.d.file(&file, 1);
                if across {
                    let at_b = self.project(&join(&w, b"b"), b, true, &mut rng, 0);
                    self.d.plant(&at_b, rule(b).id);
                    self.d.hardlink(join(&at_b, b"h2"), &file);
                } else {
                    self.d.hardlink(join(&at_a, b"h2"), &file);
                }
                if outside {
                    self.d.hardlink(join(&w, b"h3"), &file);
                }
            }
            Part::KeepDeep { rule, depth } => {
                let mut at = self.project(&w, rule, true, &mut rng, 2);
                for k in 0..depth {
                    at = join(&at, format!("k{k}").as_bytes());
                }
                self.d.file(join(&at, KEEP_MARKER.as_bytes()), 0);
            }
            Part::KeepAbove { rule, depth } => {
                self.d.file(join(&w, KEEP_MARKER.as_bytes()), 0);
                let mut dir = w;
                for k in 0..depth {
                    dir = join(&dir, format!("n{k}").as_bytes());
                }
                self.project(&dir, rule, true, &mut rng, 2);
            }
            Part::Nested { outer, inner } => {
                let at = self.project(&w, outer, true, &mut rng, 2);
                self.project(&join(&at, b"p"), inner, true, &mut rng, 2);
            }
            Part::InProtected { folder, venv } => {
                let (file, text) = if venv { ("pyvenv.cfg", "home = /usr/bin\n") } else { ("CACHEDIR.TAG", TAG) };
                self.text_once(join(folder.as_bytes(), file.as_bytes()), text);
            }
            Part::AtTop { rule, git } => {
                let at = self.project(&w, rule, true, &mut rng, 2);
                if git {
                    self.d.file(join(&at, b".git/HEAD"), 0);
                } else {
                    self.d.file(join(&at, b"package.json"), 0);
                }
            }
            Part::EntryKept { folder, top } => {
                let entry = join(folder.as_bytes(), format!("k{i}").as_bytes());
                let at = if top { entry.clone() } else { join(&entry, b"x") };
                self.d.file(join(&at, KEEP_MARKER.as_bytes()), 0);
                self.d.file(join(&entry, b"f"), 1);
            }
        }
    }
}

/// A built tree, its scan's environment and the scratch it lives in.
struct Tree {
    desc: Desc,
    layout: Layout,
    /// The tree root: the home and the only scan root.
    home: PathBuf,
    /// A folder beside the tree that symlinks point to; nothing may touch it.
    outside: PathBuf,
    env: Env,
    catalog: Catalog,
    // Last: removed after everything above is dropped.
    _scratch: Scratch,
}

/// The scan's clock: an hour after the build, so a fresh file is active
/// (younger than `min_age`) but not a recent entry (older than `RECENT`).
const LATER: Duration = Duration::from_secs(3600);

fn build_tree(parts: &[Part]) -> Tree {
    let scratch = Scratch::new().unwrap();
    let layout = target::prepare(scratch.path(), None, 1_000).unwrap();
    let outside = layout.data.join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("precious"), b"not ddc's").unwrap();
    let mut b = Builder {
        d: Desc::new(),
        files: HashSet::new(),
        homes: HashSet::new(),
        cloud: HashSet::new(),
        at_home: false,
        outside: outside.as_os_str().as_bytes(),
    };
    for (i, part) in parts.iter().enumerate() {
        b.place(i, part);
    }
    let mut desc = b.d.finish();
    desc.home = Some(Vec::new());
    build::build(&desc, &layout.tree).unwrap();
    let home = layout.tree.clone();
    let now = SystemTime::now() + LATER;
    let env =
        Env::with_paths(&home, &layout.data, &layout.data.join("logs"), now).unwrap().with_system_root(&layout.data);
    let catalog = Catalog::builtin(env.home());
    Tree { desc, layout, home, outside, env, catalog, _scratch: scratch }
}

/// One volume for every path: the scan's volume list is not under test.
struct OneVolume;

impl StatFs for OneVolume {
    fn statfs(&self, _: &Path) -> io::Result<VolumeStats> {
        Ok(VolumeStats {
            fs_type: "apfs".to_owned(),
            mount_point: PathBuf::from("/"),
            available_bytes: 1 << 40,
            total_bytes: 2 << 40,
            free_bytes: 1 << 40,
            mounted_from: "/dev/fake".to_owned(),
        })
    }
}

fn scan_tree(t: &Tree) -> (ScanReport, EntryIndex) {
    let opts = ScanOptions {
        roots: vec![t.home.clone()],
        // Every file with bytes outside a finding is a large file.
        large_file_bytes: BLOCK,
        walk: WalkOptions { reader: ReaderKind::Std, ..WalkOptions::default() },
        ..ScanOptions::default()
    };
    report::scan_with_entries(&t.env, &opts, &Progress::default(), &OneVolume).unwrap()
}

/// The tree as paths, for a failure message.
fn listing(desc: &Desc) -> String {
    desc.nodes.iter().map(|n| String::from_utf8_lossy(&n.path).into_owned()).collect::<Vec<_>>().join("\n  ")
}

// ---- file system facts, read independently of the core ---------------------------

/// A `.ddc-keep` of any kind at any depth inside `dir`, `dir` included.
fn holds_keep(dir: &Path) -> bool {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| e.file_name() == KEEP_MARKER || (e.file_type().is_ok_and(|t| t.is_dir()) && holds_keep(&e.path())))
}

/// A `.ddc-keep` in a folder above `path`, up to and including `root`.
fn keep_above(path: &Path, root: &Path) -> bool {
    path.ancestors().skip(1).take_while(|a| a.starts_with(root)).any(|a| a.join(KEEP_MARKER).symlink_metadata().is_ok())
}

/// `*` matches any text, as in the catalog's lock-file patterns.
fn glob(pattern: &[u8], name: &[u8]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|i| glob(rest, &name[i..])),
        Some((c, rest)) => name.first() == Some(c) && glob(rest, &name[1..]),
    }
}

fn has_lock(dir: &Path, patterns: &[&str]) -> bool {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|e| patterns.iter().any(|p| glob(p.as_bytes(), e.file_name().as_bytes())))
}

/// Every path below `dir`, symlinks not followed.
fn all_paths(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = e.path();
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            all_paths(&path, out);
        }
        out.push(path);
    }
}

/// Lists like [`StdReader`] and records every folder it opens.
#[derive(Default)]
struct Counting {
    read: Mutex<Vec<PathBuf>>,
}

impl DirReader for Counting {
    fn read(
        &self,
        dir: &Path,
        expect: Option<(u64, u64)>,
        want_private: bool,
        times: Times,
    ) -> Result<DirListing, ReadError> {
        self.read.lock().unwrap().push(dir.to_path_buf());
        StdReader.read(dir, expect, want_private, times)
    }
}

// ---- the safety property ---------------------------------------------------------

/// No item is a protected folder, a scan root or an ancestor of one; no
/// Delete or Trash item is at, in or above a VM rule's path; none holds a
/// `.ddc-keep` or has one above it inside the root.
fn plan_is_safe(t: &Tree, plan: &Plan, which: &str) -> Result<(), TestCaseError> {
    let (in_home, system) = protected();
    let guarded: Vec<PathBuf> = in_home
        .iter()
        .map(|rel| t.home.join(rel))
        .chain(system.iter().map(PathBuf::from))
        .chain(plan.roots.iter().cloned())
        .collect();
    let vms: Vec<PathBuf> = VM.iter().flat_map(|id| anchored(rule(id))).map(|p| t.home.join(p)).collect();
    for item in &plan.items {
        let p = &item.finding.path;
        for g in &guarded {
            prop_assert!(
                !g.starts_with(p),
                "{which}: {} ({}) is or holds the protected {}",
                p.display(),
                item.finding.rule,
                g.display()
            );
        }
        if matches!(item.action, Action::Delete { .. } | Action::Trash | Action::TrashEntry) {
            for vm in &vms {
                prop_assert!(
                    !p.starts_with(vm) && !vm.starts_with(p),
                    "{which}: {} ({}) would remove the VM folder {}",
                    p.display(),
                    item.action.name(),
                    vm.display()
                );
            }
        }
        prop_assert!(!holds_keep(p), "{which}: {} ({}) holds a {KEEP_MARKER}", p.display(), item.finding.rule);
        prop_assert!(
            !keep_above(p, &t.home),
            "{which}: {} ({}) has a {KEEP_MARKER} above it",
            p.display(),
            item.finding.rule
        );
        prop_assert!(item.finding.class != RiskClass::UserData, "{which}: user data {} is planned", p.display());
    }
    Ok(())
}

/// The default selection of the safety ADR (§1, §3) over every finding.
fn selection_holds(t: &Tree, f: &Finding) -> Result<(), TestCaseError> {
    let r = t.catalog.by_id(&f.rule).expect("a finding names a catalog rule");
    let path = f.path.display();
    if f.selected {
        prop_assert!(!f.ignored, "{path}: selected and ignored");
        prop_assert!(matches!(f.class, RiskClass::Safe | RiskClass::Rebuildable), "{path}: selected {:?}", f.class);
    }
    let parent = f.path.parent().expect("a finding has a parent");
    let root = f.project.as_ref().map(|p| p.root.as_path());
    let locked = r.lockfiles.map(|locks| [Some(parent), root].into_iter().flatten().any(|d| has_lock(d, locks)));
    if locked == Some(false) {
        prop_assert!(f.class != RiskClass::Rebuildable, "{path}: rebuildable with no lock file");
    }
    if f.selected && f.class == RiskClass::Rebuildable {
        let top = f.path.strip_prefix(&t.home).ok().and_then(|rel| rel.components().next());
        let top = top.map(|c| c.as_os_str().as_bytes().to_vec()).unwrap_or_default();
        prop_assert!(!top.starts_with(b".") && top != b"Library", "{path}: selected, but an app's");
        let project = f.project.as_ref();
        prop_assert!(project.is_some_and(|p| p.last_activity.is_some()), "{path}: selected with no known activity");
        let min_days = ScanOptions::default().min_age.as_secs() / 86_400;
        prop_assert!(
            project.and_then(|p| p.idle_days).is_some_and(|d| d >= min_days),
            "{path}: selected, active within {min_days} days"
        );
    }
    Ok(())
}

/// Neither cloud folder is opened, and nothing in one is reported.
fn clouds_untouched(t: &Tree, report: &ScanReport, index: &EntryIndex) -> Result<(), TestCaseError> {
    let clouds: Vec<PathBuf> = CLOUD.iter().map(|p| t.home.join(p)).collect();
    let under = |p: &Path| clouds.iter().any(|c| p.starts_with(c));
    for f in &report.findings {
        prop_assert!(!under(&f.path), "finding in a cloud folder: {}", f.path.display());
    }
    for l in &report.large_files {
        prop_assert!(!under(&l.path), "large file in a cloud folder: {}", l.path.display());
    }
    for (folder, indexed) in &index.folders {
        for name in indexed.entries.keys() {
            prop_assert!(!under(&folder.join(name)), "entry in a cloud folder: {}", folder.join(name).display());
        }
    }
    let reader = Counting::default();
    let visitor = RuleVisitor::new(&t.catalog, Vec::new());
    scan::walk_with_reader(
        std::slice::from_ref(&t.home),
        &visitor,
        &WalkOptions::default(),
        &Progress::default(),
        &reader,
    )
    .unwrap();
    let read = reader.read.into_inner().unwrap();
    prop_assert!(read.contains(&t.home), "the walk did not open the home");
    for dir in &read {
        prop_assert!(!under(dir), "the walk opened {}", dir.display());
    }
    Ok(())
}

// ---- the user-data property -----------------------------------------------------

/// How a generated list names a pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    /// An entry to the Trash, as the app lists it.
    Trash,
    /// An entry as `delete`.
    Delete,
    /// An entry to the Trash, but `permanent`.
    Permanent,
    /// The user's folder itself.
    Folder,
}

fn pick() -> impl Strategy<Value = Pick> {
    prop_oneof![4 => Just(Pick::Trash), 1 => Just(Pick::Delete), 1 => Just(Pick::Permanent), 1 => Just(Pick::Folder)]
}

/// The list item for one pick among the indexed entries, and what the index says of it.
fn listed(index: &EntryIndex, at: Index, how: Pick) -> Option<(ListedItem, Option<EntryState>)> {
    let mut entries: Vec<(PathBuf, &str, u64, u64, Option<EntryState>)> = Vec::new();
    for (folder, f) in &index.folders {
        if how == Pick::Folder {
            entries.push((folder.clone(), &f.rule, f.dev, f.ino, None));
        } else {
            for (name, e) in &f.entries {
                entries.push((folder.join(name), &f.rule, e.dev, e.ino, Some(e.state.clone())));
            }
        }
    }
    if entries.is_empty() {
        return None;
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let (path, rule, dev, ino, state) = entries.swap_remove(at.index(entries.len()));
    let (action, permanent) = match how {
        Pick::Delete => (ActionKind::Delete, false),
        Pick::Permanent => (ActionKind::Trash, true),
        Pick::Trash | Pick::Folder => (ActionKind::Trash, false),
    };
    let item = ListedItem { path, dev, ino, rule: rule.to_owned(), class: RiskClass::UserData, action, permanent };
    Some((item, state))
}

// ---- the executor property --------------------------------------------------------

/// No tool is installed: native items fall back or are skipped.
struct NoTools;

impl CommandRunner for NoTools {
    fn find(&self, _: &str) -> Option<PathBuf> {
        None
    }
    fn at(&self, _: &Path) -> Option<PathBuf> {
        None
    }
    fn run(&self, exe: &Path, _: &[String], _: &[(&str, &str)], _: Duration) -> io::Result<CmdOutput> {
        Err(io::Error::other(format!("no tool runs here: {}", exe.display())))
    }
}

struct NoProcesses;

impl ProcessTable for NoProcesses {
    fn processes(&self) -> Vec<ProcInfo> {
        Vec::new()
    }
    fn open_files(&self, _: i32) -> Vec<PathBuf> {
        Vec::new()
    }
    fn process_start(&self, _: i32) -> Option<ProcStart> {
        None
    }
    fn open_vnodes(&self, _: i32) -> Vec<OpenVnode> {
        Vec::new()
    }
}

/// Moves into a folder of the scratch, under a new name each time.
struct FakeTrash {
    dir: PathBuf,
    moved: Cell<u32>,
}

impl Trasher for FakeTrash {
    fn trash(&self, path: &Path) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let n = self.moved.get();
        self.moved.set(n + 1);
        fs::rename(path, self.dir.join(n.to_string()))
    }
}

/// A plan item for `path` as the scan would describe it, with its real identity.
fn injected(path: &Path, r: &'static Rule, class: RiskClass, action: Action) -> PlanItem {
    let id = |p: &Path| p.symlink_metadata().map(|m| (m.dev(), m.ino())).ok();
    let (dev, ino) = id(path).unwrap_or_default();
    let up = path.parent().and_then(id);
    let finding = Finding {
        path: path.to_path_buf(),
        rule: r.id.to_owned(),
        category: r.category,
        class,
        rule_class: r.class,
        removal: match action {
            Action::Delete { .. } => RemovalKind::Delete,
            Action::Trash | Action::TrashEntry => RemovalKind::Trash,
            Action::Native { .. } => RemovalKind::Native,
            Action::NativeOnly(_) => RemovalKind::NativeOnly,
        },
        bytes: 0,
        shared_bytes: 0,
        private_bytes: None,
        files: 0,
        dirs: 0,
        incomplete: false,
        project: None,
        selected: true,
        not_selected: None,
        ignored: false,
        notes: Vec::new(),
        hint: r.hint.to_owned(),
        dev,
        ino,
        parent_dev: up.map(|u| u.0),
        parent_ino: up.map(|u| u.1),
        contents: None,
        protection: None,
        modified: None,
        origin: r.origin,
        worktree: None,
    };
    PlanItem { finding, action, default_selected: false, selected: true }
}

// ---- the properties --------------------------------------------------------------
//
// Each property skips where the platform is not supported before its
// strategies are built: they draw from the catalog's cloud and entry folders,
// which another platform's catalog may lack, and an empty `select` panics.

/// For any tree: the default plan and the plan with every item selected
/// (Trash and permanent) hold no protected folder, scan root, VM folder
/// or kept folder; nothing in a cloud folder is opened or reported; and
/// the default selection holds over every finding.
#[test]
fn no_plan_holds_a_protected_folder_a_vm_or_a_kept_folder() {
    if ddc_loadgen::unsupported("no_plan_holds_a_protected_folder_a_vm_or_a_kept_folder") {
        return;
    }
    proptest!(config(), |(parts in any_tree())| {
        let t = build_tree(&parts);
        let (report, index) = scan_tree(&t);
        let tree = listing(&t.desc);
        let default = build_plan(&report, &t.catalog, &PlanOptions::default());
        for item in &default.items {
            prop_assert!(item.selected == item.finding.selected && item.default_selected == item.finding.selected);
        }
        plan_is_safe(&t, &default, "default plan").map_err(|e| with_tree(e, &tree))?;
        for permanent in [false, true] {
            let mut all = build_plan(&report, &t.catalog, &PlanOptions { permanent, ..PlanOptions::default() });
            all.items.iter_mut().for_each(|i| i.selected = true);
            plan_is_safe(&t, &all, if permanent { "all selected, permanent" } else { "all selected" })
                .map_err(|e| with_tree(e, &tree))?;
        }
        for f in &report.findings {
            selection_holds(&t, f).map_err(|e| with_tree(e, &tree))?;
        }
        clouds_untouched(&t, &report, &index).map_err(|e| with_tree(e, &tree))?;
    });
}

/// User data enters a plan only as an entry the list names, to the
/// Trash: a listed folder, an entry listed as `delete` or `permanent`
/// is refused, and so is a locked one, with its lock.
#[test]
fn user_data_is_planned_only_as_a_listed_entry_to_the_trash() {
    if ddc_loadgen::unsupported("user_data_is_planned_only_as_a_listed_entry_to_the_trash") {
        return;
    }
    proptest!(config(), |(
        parts in any_tree(),
        folders in prop::collection::vec(select(ENTRY_FOLDERS.clone()), 1..3),
        picks in prop::collection::vec((any::<Index>(), pick()), 1..8),
    )| {
        let mut parts = parts;
        // Every user folder holds payload entries; a kept one may hold a marker.
        parts.extend(folders.iter().map(|f| {
            let id = ENTRY_RULES.iter().copied().find(|id| anchored(rule(id))[0] == *f).unwrap();
            Part::Home { rule: id, seed: 7 }
        }));
        parts.extend(folders.iter().map(|&folder| Part::EntryKept { folder, top: false }));
        let t = build_tree(&parts);
        let tree = listing(&t.desc);
        let (report, index) = scan_tree(&t);
        let mut seen = HashSet::new();
        let list: Vec<(ListedItem, Option<EntryState>, Pick)> = picks
            .iter()
            .filter_map(|&(at, how)| listed(&index, at, how).map(|(item, state)| (item, state, how)))
            .filter(|(item, _, _)| seen.insert(item.path.clone()))
            .collect();
        let items: Vec<ListedItem> = list.iter().map(|(item, _, _)| item.clone()).collect();
        let out = plan_listed(&report, &index, &t.catalog, &items);
        let planned: BTreeMap<&Path, &PlanItem> =
            out.plan.items.iter().map(|i| (i.finding.path.as_path(), i)).collect();
        let refused: BTreeMap<&Path, &SkipReason> =
            out.refused.iter().map(|r| (r.item.path.as_path(), &r.reason)).collect();
        prop_assert_eq!(
            planned.len() + refused.len(),
            items.len(),
            "every listed item is planned or refused\n  {}",
            tree
        );
        for (item, state, how) in &list {
            let path = item.path.as_path();
            match how {
                Pick::Trash => {
                    if let Some(p) = planned.get(path) {
                        prop_assert_eq!(p.action, Action::TrashEntry);
                        prop_assert!(p.selected);
                        prop_assert_eq!(state.as_ref(), Some(&EntryState::Pickable), "{} is planned", path.display());
                        prop_assert!(!holds_keep(path), "{} holds a {KEEP_MARKER} and is planned", path.display());
                    }
                    if let Some(EntryState::Locked(lock)) = state {
                        let want = SkipReason::EntryLocked { lock: lock.clone() };
                        prop_assert_eq!(refused.get(path).copied(), Some(&want), "{}\n  {}", path.display(), tree);
                    }
                }
                Pick::Delete | Pick::Permanent | Pick::Folder => {
                    prop_assert_eq!(
                        refused.get(path).copied(),
                        Some(&SkipReason::ActionNotAllowed),
                        "{:?} {} is not refused\n  {}", how, path.display(), tree
                    );
                }
            }
        }
    });
}

/// The plan is untrusted input: an injected protected folder is skipped
/// as `protected`, a `native-only` rule's Delete or Trash item as
/// `action-not-allowed`, a listed entry holding a `.ddc-keep` as
/// `entry-locked`; and every removed path lies under an item that was
/// cleaned.
#[test]
fn the_executor_refuses_what_the_plan_must_not_hold() {
    if ddc_loadgen::unsupported("the_executor_refuses_what_the_plan_must_not_hold") {
        return;
    }
    proptest!(config(), |(
        parts in any_tree(),
        native in select(NATIVE_ONLY.clone()),
        trash in any::<bool>(),
        folder in select(ENTRY_FOLDERS.clone()),
    )| {
        let t = build_tree(&parts);
        let tree = listing(&t.desc);
        let (report, _) = scan_tree(&t);
        let mut plan = build_plan(&report, &t.catalog, &PlanOptions::default());
        plan.items.iter_mut().for_each(|i| i.selected = true);

        let mut first = Vec::new();
        let mut want = Vec::new();
        let tag = cachedir_rule();
        for rel in protected().0.iter().filter(|rel| !rel.is_empty()) {
            let path = t.home.join(rel);
            fs::create_dir_all(&path).unwrap();
            let action = action_for(tag, tag.class, false).unwrap();
            first.push(injected(&path, tag, tag.class, action));
            want.push(SkipReason::Protected);
        }
        let vm = rule(native);
        let action = if trash { Action::Trash } else { Action::Delete { make_writable: false } };
        let vm_path = t.home.join(anchored(vm).first().copied().unwrap_or("vm"));
        first.push(injected(&vm_path, vm, vm.class, action));
        want.push(SkipReason::ActionNotAllowed);
        let entry_rule = rule(ENTRY_RULES.iter().copied().find(|id| anchored(rule(id))[0] == folder).unwrap());
        let entry = t.home.join(folder).join("kept-entry");
        fs::create_dir_all(entry.join("x")).unwrap();
        fs::write(entry.join("x").join(KEEP_MARKER), b"").unwrap();
        first.push(injected(&entry, entry_rule, RiskClass::UserData, Action::TrashEntry));
        want.push(SkipReason::EntryLocked { lock: EntryLock::Kept });
        let injected_n = first.len();
        first.append(&mut plan.items);
        plan.items = first;

        let mut before = Vec::new();
        all_paths(&t.home, &mut before);
        let trash_dir = t.layout.data.join("trash");
        let bin = FakeTrash { dir: trash_dir.clone(), moved: Cell::new(0) };
        let mut journal = MemoryJournal::default();
        let ports = Ports { commands: &NoTools, trash: &bin, procs: &NoProcesses, journal: &mut journal };
        let opts = ExecOptions { local_snapshots: Some(0), ..ExecOptions::default() };
        let run = exec::execute(&t.env, &t.catalog, &plan, &Consent::Listed, ports, &opts).unwrap();
        prop_assert_eq!(run.items.len(), plan.items.len());

        for (got, want) in run.items.iter().zip(&want) {
            prop_assert_eq!(&got.outcome, &Outcome::Skipped(want.clone()), "{}\n  {}", got.path.display(), tree);
        }
        let cleaned: Vec<&Path> = run.items[injected_n..]
            .iter()
            .filter(|i| !matches!(i.outcome, Outcome::Skipped(_) | Outcome::NotRun))
            .map(|i| i.path.as_path())
            .collect();
        for path in &before {
            if path.symlink_metadata().is_err() {
                prop_assert!(
                    cleaned.iter().any(|c| path.starts_with(c)),
                    "{} was removed, but lies under no cleaned item\n  {}", path.display(), tree
                );
            }
        }
        let trashed = run.items.iter().filter(|i| i.outcome == Outcome::Trashed).count();
        let in_bin = fs::read_dir(&trash_dir).map(|d| d.count()).unwrap_or(0);
        prop_assert_eq!(in_bin, trashed, "the Trash holds what was trashed, nothing else");
        prop_assert!(t.outside.join("precious").exists(), "a file beside the tree was removed");
    });
}

/// Sizes are counted once: the scan matches the manifest (totals,
/// each finding's bytes and shared bytes, the default plan), and the
/// class totals are the sums over the findings that are not ignored.
#[test]
fn sizes_are_counted_once() {
    if ddc_loadgen::unsupported("sizes_are_counted_once") {
        return;
    }
    proptest!(config(), |(parts in modeled_tree())| {
        let t = build_tree(&parts);
        let tree = listing(&t.desc);
        let m = manifest("properties", 0, &t.desc);
        let (report, _) = scan_tree(&t);
        let plan = build_plan(&report, &t.catalog, &PlanOptions::default());
        let diffs = verify::diffs(&m, &t.layout, &report, &plan);
        prop_assert!(diffs.is_empty(), "{} differences:\n{}\n  {}", diffs.len(), diffs.join("\n"), tree);
        for c in &report.totals.classes {
            let of: Vec<&Finding> = report.findings.iter().filter(|f| !f.ignored && f.class == c.class).collect();
            let selected: Vec<&&Finding> = of.iter().filter(|f| f.selected).collect();
            prop_assert_eq!(c.count, of.len() as u64);
            prop_assert_eq!(c.bytes, of.iter().map(|f| f.bytes).sum::<u64>());
            prop_assert_eq!(c.selected_count, selected.len() as u64);
            prop_assert_eq!(c.selected_bytes, selected.iter().map(|f| f.bytes).sum::<u64>());
        }
    });
}

/// A failure with the tree it happened on.
fn with_tree(e: TestCaseError, tree: &str) -> TestCaseError {
    match e {
        TestCaseError::Fail(why) => TestCaseError::fail(format!("{why}\n  {tree}")),
        other => other,
    }
}

#[test]
fn the_claim_checked_folders_are_protected() {
    for f in CLAIM_CHECKED {
        assert!(protected().0.contains(f), "{f} is not protected");
    }
}

/// The premises of the property tests, which skip where the platform is not
/// supported: its catalog may lack an entry folder or the cloud folders.
#[test]
fn every_entry_folder_and_vm_rule_has_a_path() {
    if ddc_loadgen::unsupported("every_entry_folder_and_vm_rule_has_a_path") {
        return;
    }
    let listed = |id: &str| RULES.iter().any(|r| r.id == id);
    assert_eq!(ENTRY_FOLDERS.len(), ENTRY_RULES.iter().filter(|id| listed(id)).count());
    assert!(!ENTRY_FOLDERS.is_empty() && !VM.is_empty() && !NATIVE_ONLY.is_empty());
    assert_eq!(CLOUD.is_empty(), !listed("user.cloud"), "the cloud folders are the user.cloud rule's");
}
