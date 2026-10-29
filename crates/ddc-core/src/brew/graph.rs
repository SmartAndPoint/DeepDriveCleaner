//! What needs what among the installed formulae, from brew's JSON alone.
//!
//! - A formula needs the runtime dependencies its kegs' tabs record, by the
//!   part of each `full_name` after the last `/` (`Formula#missing_dependencies`
//!   compares them that way: a Cellar rack holds one formula per short name).
//!   A keg whose tab records none (`runtime_dependencies` null) is read
//!   through the formula's declared `dependencies` and
//!   `recommended_dependencies`, so it neither blocks every removal nor
//!   hides what it needs.
//! - A cask needs the formulae its `depends_on formula:` names, matched
//!   against both a formula's `name` and its `full_name`.
//! - The orphans of a removal are its dependencies, recursively, that were
//!   not installed on request and that nothing left installed needs: the
//!   largest such set, so a cycle among them goes with them. A pinned one is
//!   kept, and so is what it needs.

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::info::Installed;
use super::short;

/// A formula with every field the graph needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// The Cellar rack's name.
    pub name: String,
    pub full_name: String,
    pub pinned: bool,
    /// One of its kegs was installed on request (an upgraded dependency
    /// carries the flag over).
    pub on_request: bool,
    /// `installed[].version`, in brew's order: `Cellar/<name>/<version>`.
    pub versions: Vec<String>,
    /// The short names its kegs need, together.
    pub needs: BTreeSet<String>,
}

/// What still needs one formula: other formulae, by `full_name`, and casks.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NeededBy {
    /// The formula that is needed, by its `full_name`.
    pub formula: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub formulae: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub casks: Vec<String>,
}

impl NeededBy {
    pub fn is_empty(&self) -> bool {
        self.formulae.is_empty() && self.casks.is_empty()
    }
}

/// What a removal leaves needed by nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Orphaned {
    pub orphans: BTreeSet<usize>,
    /// Pinned: they would be orphans, and are kept.
    pub kept_pinned: BTreeSet<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct Graph {
    nodes: Vec<Node>,
    by_name: BTreeMap<String, usize>,
    /// For each node, the nodes that need it.
    needers: Vec<Vec<usize>>,
    /// For each node, the casks that need it.
    cask_needers: Vec<Vec<String>>,
    /// The formulae brew listed without a field the graph needs, by the
    /// name brew gave them, if any. While one is there, what it needs is
    /// not known, so no removal can be judged.
    pub incomplete: Vec<String>,
}

impl Graph {
    pub fn new(installed: &Installed) -> Self {
        let mut nodes = Vec::with_capacity(installed.formulae.len());
        let mut incomplete = Vec::new();
        for f in &installed.formulae {
            let versions: Option<Vec<String>> =
                f.kegs.as_ref().and_then(|kegs| kegs.iter().map(|k| k.version.clone()).collect());
            let (Some(name), Some(full_name), Some(pinned), Some(versions), Some(kegs)) =
                (&f.name, &f.full_name, f.pinned, versions, &f.kegs)
            else {
                incomplete
                    .push(f.full_name.clone().or_else(|| f.name.clone()).unwrap_or_else(|| "(no name)".to_owned()));
                continue;
            };
            let declared: Vec<&String> = f.dependencies.iter().chain(&f.recommended_dependencies).collect();
            let mut needs = BTreeSet::new();
            for keg in kegs {
                match &keg.runtime_dependencies {
                    Some(deps) => needs.extend(deps.iter().map(|d| short(d).to_owned())),
                    None => needs.extend(declared.iter().map(|d| short(d).to_owned())),
                }
            }
            needs.remove(name.as_str());
            nodes.push(Node {
                name: name.clone(),
                full_name: full_name.clone(),
                pinned,
                on_request: kegs.iter().any(|k| k.installed_on_request),
                versions,
                needs,
            });
        }
        let by_name: BTreeMap<String, usize> = nodes.iter().enumerate().map(|(i, n)| (n.name.clone(), i)).collect();
        let mut needers = vec![Vec::new(); nodes.len()];
        for (j, n) in nodes.iter().enumerate() {
            for need in &n.needs {
                if let Some(&i) = by_name.get(need) {
                    needers[i].push(j);
                }
            }
        }
        let cask_needers = nodes
            .iter()
            .map(|n| {
                installed
                    .casks
                    .iter()
                    .filter(|c| c.formulae.iter().any(|f| *f == n.name || *f == n.full_name))
                    .map(|c| c.token.clone())
                    .collect()
            })
            .collect();
        Self { nodes, by_name, needers, cask_needers, incomplete }
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// The formula whose `full_name` is exactly `full_name`: a tap formula
    /// and a core one of the same short name are never taken for each other.
    pub fn find(&self, full_name: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.full_name == full_name)
    }

    /// The installed formula of the rack `name`.
    pub fn by_name(&self, name: &str) -> Option<usize> {
        self.by_name.get(name).copied()
    }

    /// What needs `i`, the formulae in `leaving` left out (they go with it).
    pub fn needed_by(&self, i: usize, leaving: &BTreeSet<usize>) -> NeededBy {
        let mut formulae: Vec<String> =
            self.needers[i].iter().filter(|j| !leaving.contains(j)).map(|&j| self.nodes[j].full_name.clone()).collect();
        formulae.sort();
        let mut casks = self.cask_needers[i].clone();
        casks.sort();
        NeededBy { formula: self.nodes[i].full_name.clone(), formulae, casks }
    }

    /// Everything `i` needs, recursively, among the installed formulae.
    pub fn closure(&self, i: usize) -> BTreeSet<usize> {
        let mut seen = BTreeSet::new();
        let mut todo = vec![i];
        while let Some(at) = todo.pop() {
            for need in &self.nodes[at].needs {
                if let Some(&d) = self.by_name.get(need)
                    && d != i
                    && seen.insert(d)
                {
                    todo.push(d);
                }
            }
        }
        seen
    }

    /// The largest part of `set` that nothing outside `removed` and the
    /// part itself needs: a member a remaining formula or a cask needs
    /// stays, and then so does what it needs.
    fn settle(&self, removed: &BTreeSet<usize>, mut set: BTreeSet<usize>) -> BTreeSet<usize> {
        loop {
            let stays: Vec<usize> = set
                .iter()
                .copied()
                .filter(|&s| {
                    !self.cask_needers[s].is_empty()
                        || self.needers[s].iter().any(|j| !removed.contains(j) && !set.contains(j))
                })
                .collect();
            if stays.is_empty() {
                return set;
            }
            for s in stays {
                set.remove(&s);
            }
        }
    }

    /// What removing `i` orphans: its dependencies, recursively, not
    /// installed on request, needed by nothing that stays.
    pub fn would_orphan(&self, i: usize) -> Orphaned {
        let removed = BTreeSet::from([i]);
        let candidates: BTreeSet<usize> = self.closure(i).into_iter().filter(|&d| !self.nodes[d].on_request).collect();
        let unpinned: BTreeSet<usize> = candidates.iter().copied().filter(|&d| !self.nodes[d].pinned).collect();
        let kept_pinned = self.settle(&removed, candidates).into_iter().filter(|&d| self.nodes[d].pinned).collect();
        Orphaned { orphans: self.settle(&removed, unpinned), kept_pinned }
    }

    /// What is orphaned now: installed, not on request, unpinned, and
    /// needed by nothing outside the set.
    pub fn orphans(&self) -> BTreeSet<usize> {
        let set = (0..self.nodes.len()).filter(|&d| !self.nodes[d].on_request && !self.nodes[d].pinned).collect();
        self.settle(&BTreeSet::new(), set)
    }
}
