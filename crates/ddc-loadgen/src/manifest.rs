//! What a scan of a tree must report and what a dry run must plan, derived
//! from its [`Desc`] alone. It never runs the scanner or the classifier: it
//! repeats only the rules that depend on the tree around a finding (the
//! app-owned and no-lock-file downgrades, project activity against
//! `min_age`), so a change to either side shows up as a difference.

use std::collections::{BTreeMap, HashMap, HashSet};

use ddc_core::class::{Category, RiskClass};
use ddc_core::plan::action_for;
use ddc_core::rules::{Catalog, KEPT, Rule};

use crate::desc::{Age, BLOCK, Desc, FsCaps, Kind, join, name, parent, within};

/// Files whose modification time marks activity in a project root
/// (`report/project.rs`, `SIGNALS`).
const SIGNALS: &[&str] = &[
    "package.json",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
    "pyproject.toml",
    "uv.lock",
    "poetry.lock",
    "Pipfile",
    "Pipfile.lock",
    "setup.py",
    "setup.cfg",
    "Cargo.toml",
    "Cargo.lock",
    "go.mod",
    "go.sum",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "Podfile",
    "Podfile.lock",
    "CMakeLists.txt",
    "Gemfile",
    "Gemfile.lock",
    "composer.json",
];

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Manifest {
    pub profile: String,
    pub seed: u64,
    /// Entries below the tree root.
    pub entries: u64,
    /// The home the manifest assumes, tree-relative; `None` = an empty home beside the tree.
    pub home: Option<String>,
    /// Only for a profile whose tree depends on the file system (`names`).
    pub fs_caps: Option<FsCaps>,
    pub totals: Totals,
    /// Expected findings per rule.
    pub rules: BTreeMap<String, u64>,
    pub findings: Vec<Expected>,
    /// What `clean --dry-run` plans with the default settings.
    pub plan: Vec<Planned>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Totals {
    pub files: u64,
    /// The tree root included, as the scan counts it.
    pub dirs: u64,
    pub scanned_bytes: u64,
    pub errors: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Expected {
    /// Tree-relative; bytes that are not UTF-8 are written `\xNN`.
    pub path: String,
    pub rule: String,
    pub class: RiskClass,
    pub selected: bool,
    pub bytes: u64,
    pub shared_bytes: u64,
    pub files: u64,
    /// The finding's own directory included.
    pub dirs: u64,
    pub incomplete: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Planned {
    pub path: String,
    pub rule: String,
    pub action: String,
    pub selected: bool,
}

impl Manifest {
    /// FNV-1a over the canonical JSON: the same tree gives the same hash on
    /// every machine and every run.
    pub fn hash(&self) -> String {
        format!("{:016x}", fnv1a(&serde_json::to_vec(self).expect("a manifest serializes")))
    }
}

/// 64-bit FNV-1a.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// A tree-relative path as text: UTF-8 as is, other bytes as `\xNN`.
pub fn show(path: &[u8]) -> String {
    let mut out = String::new();
    for chunk in path.utf8_chunks() {
        out.push_str(chunk.valid());
        for b in chunk.invalid() {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    out
}

pub fn manifest(profile: &str, seed: u64, desc: &Desc) -> Manifest {
    let tree = Index::new(desc);
    let catalog_home = std::path::Path::new("/");
    let catalog = Catalog::builtin(catalog_home);
    let mut findings: Vec<Expected> =
        desc.planted.iter().map(|p| tree.expect(&p.path, rule(&catalog, p.rule))).collect();
    findings.sort_by(|a, b| a.path.cmp(&b.path));
    let mut rules = BTreeMap::new();
    for f in &findings {
        *rules.entry(f.rule.clone()).or_insert(0) += 1;
    }
    let plan = findings
        .iter()
        .filter(|f| f.class != RiskClass::UserData && !f.path.is_empty())
        .filter_map(|f| {
            let action = action_for(rule(&catalog, &f.rule), f.class, false)?;
            Some(Planned {
                path: f.path.clone(),
                rule: f.rule.clone(),
                action: action.name().to_owned(),
                selected: f.selected,
            })
        })
        .collect();
    Manifest {
        profile: profile.to_owned(),
        seed,
        entries: desc.nodes.len() as u64,
        home: desc.home.as_deref().map(show),
        fs_caps: desc.caps,
        totals: tree.totals(),
        rules,
        findings,
        plan,
    }
}

fn rule(catalog: &Catalog, id: &str) -> &'static Rule {
    catalog.by_id(id).unwrap_or_else(|| panic!("no rule {id} in the catalog"))
}

/// The description, indexed for the expectations.
struct Index<'d> {
    desc: &'d Desc,
    ages: HashMap<&'d [u8], Age>,
    kinds: HashMap<&'d [u8], &'d Kind>,
    pos: HashMap<&'d [u8], usize>,
    children: HashMap<&'d [u8], Vec<&'d [u8]>>,
    /// Every name of each file, keyed by its first name.
    links: HashMap<&'d [u8], Vec<&'d [u8]>>,
    dirs: HashSet<&'d [u8]>,
}

impl<'d> Index<'d> {
    fn new(desc: &'d Desc) -> Self {
        let mut ix = Self {
            desc,
            ages: HashMap::new(),
            kinds: HashMap::new(),
            pos: HashMap::new(),
            children: HashMap::new(),
            links: HashMap::new(),
            dirs: HashSet::new(),
        };
        for (i, n) in desc.nodes.iter().enumerate() {
            ix.pos.insert(&n.path, i);
            ix.ages.insert(&n.path, n.age);
            ix.kinds.insert(&n.path, &n.kind);
            ix.children.entry(parent(&n.path).unwrap_or_default()).or_default().push(name(&n.path));
            match &n.kind {
                Kind::Dir => {
                    ix.dirs.insert(&n.path);
                }
                Kind::File { .. } | Kind::Text(_) => ix.links.entry(&n.path).or_default().push(&n.path),
                Kind::HardLink(target) => ix.links.entry(target.as_slice()).or_default().push(&n.path),
                Kind::Symlink(_) => {}
            }
        }
        ix
    }

    fn exists(&self, path: &[u8]) -> bool {
        self.ages.contains_key(path)
    }

    fn alloc(kind: &Kind) -> u64 {
        match kind {
            Kind::File { blocks } => u64::from(*blocks) * BLOCK,
            Kind::Text(t) if !t.is_empty() => BLOCK,
            _ => 0,
        }
    }

    /// Whether the scan sees `path`: not in a folder it never opens, nor
    /// below one it cannot read.
    fn seen(&self, path: &[u8]) -> bool {
        !self.desc.unopened.iter().any(|u| within(path, u))
            && !self.desc.unreadable.iter().any(|u| path != u.as_slice() && within(path, u))
    }

    fn totals(&self) -> Totals {
        let nodes: Vec<_> = self.desc.nodes.iter().filter(|n| self.seen(&n.path)).collect();
        Totals {
            files: nodes.iter().filter(|n| n.kind != Kind::Dir).count() as u64,
            dirs: nodes.iter().filter(|n| n.kind == Kind::Dir).count() as u64 + 1,
            scanned_bytes: nodes.iter().map(|n| Self::alloc(&n.kind)).sum(),
            errors: self.desc.unreadable.iter().filter(|u| self.seen(u)).count() as u64,
        }
    }

    fn expect(&self, path: &[u8], rule: &'static Rule) -> Expected {
        // The nodes are sorted depth-first: a subtree is one run of them.
        let start = self.pos.get(path).copied().unwrap_or(self.desc.nodes.len());
        let inside: Vec<_> = self.desc.nodes[start..]
            .iter()
            .take_while(|n| within(&n.path, path))
            .filter(|n| self.seen(&n.path))
            .collect();
        let files: HashSet<&[u8]> = inside
            .iter()
            .filter_map(|n| match &n.kind {
                Kind::File { .. } | Kind::Text(_) => Some(n.path.as_slice()),
                Kind::HardLink(target) => Some(target.as_slice()),
                _ => None,
            })
            .collect();
        let (mut bytes, mut shared_bytes) = (0, 0);
        // A file counts in full when all its names are inside, else as shared.
        for first in files {
            let names = &self.links[first];
            let alloc = Self::alloc(self.kinds[first]);
            if names.iter().all(|l| within(l, path)) {
                bytes += alloc;
            } else {
                shared_bytes += alloc;
            }
        }
        let (class, selected) = self.class(path, rule);
        Expected {
            path: show(path),
            rule: rule.id.to_owned(),
            class,
            selected,
            bytes,
            shared_bytes,
            files: inside.iter().filter(|n| n.kind != Kind::Dir).count() as u64,
            dirs: inside.iter().filter(|n| n.kind == Kind::Dir).count() as u64,
            incomplete: self.desc.unreadable.iter().any(|u| within(u, path) && self.seen(u)),
        }
    }

    /// The effective class and the default selection (`rules/classify.rs`
    /// `downgraded`, `report/mod.rs` `finding`).
    fn class(&self, path: &[u8], rule: &'static Rule) -> (RiskClass, bool) {
        let up = parent(path).unwrap_or_default();
        let root = (rule.category == Category::ProjectArtifact && rule.id != KEPT).then(|| self.project_root(up));
        let mut class = rule.class;
        if class == RiskClass::Rebuildable && self.app_owned(path) {
            class = RiskClass::Review;
        }
        if class == RiskClass::Rebuildable
            && let Some(locks) = rule.lockfiles
            && ![Some(up), root].into_iter().flatten().any(|d| self.has_any(d, locks))
        {
            class = RiskClass::Review;
        }
        let selected = match class {
            RiskClass::Safe => true,
            RiskClass::Rebuildable => root.is_some_and(|root| {
                let mut ages = self.activity(root);
                if up != root {
                    ages.extend(self.activity(up));
                }
                !ages.is_empty() && !ages.contains(&Age::Fresh)
            }),
            RiskClass::Review | RiskClass::UserData => false,
        };
        (class, selected)
    }

    /// Nearest ancestor with `.git`, not the home, up to the tree root; else `dir`.
    fn project_root<'p>(&self, dir: &'p [u8]) -> &'p [u8] {
        let mut d = dir;
        loop {
            if Some(d) != self.desc.home.as_deref() && self.exists(&join(d, b".git")) {
                return d;
            }
            match parent(d) {
                Some(p) => d = p,
                None if !d.is_empty() => d = b"",
                None => return dir,
            }
        }
    }

    fn app_owned(&self, path: &[u8]) -> bool {
        let Some(home) = self.desc.home.as_deref() else { return false };
        if path == home || !within(path, home) {
            return false;
        }
        let rel = &path[home.len() + usize::from(!home.is_empty())..];
        let first = rel.split(|&c| c == b'/').next().unwrap_or_default();
        first.starts_with(b".") || first == b"Library"
    }

    fn has_any(&self, dir: &[u8], patterns: &[&str]) -> bool {
        self.children.get(dir).is_some_and(|names| names.iter().any(|n| patterns.iter().any(|p| glob(p.as_bytes(), n))))
    }

    /// Ages of the activity signals in `dir`.
    fn activity(&self, dir: &[u8]) -> Vec<Age> {
        let mut paths: Vec<Vec<u8>> = SIGNALS.iter().map(|s| join(dir, s.as_bytes())).collect();
        if self.dirs.contains(join(dir, b".git").as_slice()) {
            paths.extend([join(dir, b".git/index"), join(dir, b".git/HEAD")]);
        }
        let requirements = self.children.get(dir).into_iter().flatten().filter(|n| glob(b"requirements*.txt", n));
        paths.extend(requirements.map(|n| join(dir, n)));
        paths.iter().filter_map(|p| self.ages.get(p.as_slice()).copied()).collect()
    }
}

/// `*` matches any text, as in the catalog's lock-file patterns.
fn glob(pattern: &[u8], name: &[u8]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => (0..=name.len()).any(|i| glob(rest, &name[i..])),
        Some((c, rest)) => name.first() == Some(c) && glob(rest, &name[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_like_the_catalog() {
        assert!(glob(b"requirements*.txt", b"requirements.txt"));
        assert!(glob(b"requirements*.txt", b"requirements-dev.txt"));
        assert!(!glob(b"requirements*.txt", b"requirements.toml"));
        assert!(glob(b"uv.lock", b"uv.lock"));
    }

    #[test]
    fn show_escapes_bytes_that_are_not_utf8() {
        assert_eq!(show(b"a/\xff\xfeb"), "a/\\xff\\xfeb");
    }
}
