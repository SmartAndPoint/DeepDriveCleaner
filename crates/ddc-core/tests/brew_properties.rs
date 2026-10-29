//! The orphans of Homebrew's dependency graph against a simple reference
//! (story "Coverage floor and property tests for the safety invariants",
//! DDC-005). Random installs are written as `brew info --json=v2
//! --installed` prints them and read through [`info::parse`] and
//! [`Graph::new`], as `ddc disk brew-packages` and the re-read before `brew
//! uninstall` read them. An orphan set never names a formula that a formula
//! left installed or a cask still needs, and it is the largest such set.

use std::collections::{BTreeMap, BTreeSet};

use ddc_core::brew::{Graph, info};
use proptest::prelude::*;
use serde_json::{Value, json};

/// 256 cases unless `PROPTEST_CASES` says otherwise (proptest's default, read
/// from the environment); shrinking stops after 30 s unless
/// `PROPTEST_MAX_SHRINK_TIME` sets it.
fn config() -> ProptestConfig {
    let base = ProptestConfig::default();
    let max_shrink_time = if base.max_shrink_time == 0 { 30_000 } else { base.max_shrink_time };
    ProptestConfig { max_shrink_time, ..base }
}

/// At most this many formulae are installed; a dependency may also name
/// one of [`GHOSTS`] formulae that are not.
const MAX_FORMULAE: usize = 10;
const GHOSTS: usize = 2;

/// How a dependency or a cask spells the formula it names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spelling {
    /// `foo`.
    Short,
    /// The formula's own `full_name`: `user/tap/foo` for a tap formula.
    Full,
    /// `other/tap/foo`: another tap's formula of the same rack name.
    OtherTap,
}

/// A name of a formula: the installed formula `target`, or a ghost past
/// the last one.
#[derive(Debug, Clone, Copy)]
struct Dep {
    target: usize,
    spelling: Spelling,
}

#[derive(Debug, Clone)]
struct Keg {
    /// `None`: the field is absent, read as false.
    on_request: Option<bool>,
    /// `None`: a blank tab (`runtime_dependencies` null).
    runtime: Option<Vec<Dep>>,
}

#[derive(Debug, Clone)]
struct Formula {
    tap: bool,
    pinned: bool,
    kegs: Vec<Keg>,
    declared: Vec<Dep>,
    recommended: Vec<Dep>,
    /// `build_dependencies` and `optional_dependencies`: never read.
    unread: Vec<Dep>,
}

#[derive(Debug, Clone)]
struct Cask {
    formulae: Vec<Dep>,
    /// `depends_on.formula` as a lone string (only its first name).
    lone: bool,
}

#[derive(Debug, Clone)]
struct Install {
    formulae: Vec<Formula>,
    casks: Vec<Cask>,
}

fn short_name(k: usize) -> String {
    // Some racks carry an `@`, as versioned formulae do.
    if k % 3 == 2 { format!("f{k}@3") } else { format!("f{k}") }
}

impl Install {
    fn full_name(&self, k: usize) -> String {
        match self.formulae.get(k) {
            Some(f) if f.tap => format!("user/tap/{}", short_name(k)),
            Some(_) => short_name(k),
            None => format!("ghost{k}"),
        }
    }

    fn spell(&self, d: Dep) -> String {
        if d.target >= self.formulae.len() {
            return format!("ghost{}", d.target);
        }
        match d.spelling {
            Spelling::Short => short_name(d.target),
            Spelling::Full => self.full_name(d.target),
            Spelling::OtherTap => format!("other/tap/{}", short_name(d.target)),
        }
    }

    fn spell_all(&self, deps: &[Dep]) -> Vec<String> {
        deps.iter().map(|&d| self.spell(d)).collect()
    }

    /// `brew info --json=v2 --installed`.
    fn json(&self) -> String {
        let formulae: Vec<Value> = self
            .formulae
            .iter()
            .enumerate()
            .map(|(k, f)| {
                let kegs: Vec<Value> = f
                    .kegs
                    .iter()
                    .enumerate()
                    .map(|(v, keg)| {
                        let runtime = keg.runtime.as_ref().map(|deps| {
                            self.spell_all(deps)
                                .into_iter()
                                .map(|n| json!({"full_name": n, "version": "1", "declared_directly": true}))
                                .collect::<Vec<_>>()
                        });
                        let mut keg_json = json!({
                            "version": format!("1.{v}"), "used_options": [], "built_as_bottle": true,
                            "poured_from_bottle": true, "time": 1_700_000_000, "runtime_dependencies": runtime,
                        });
                        if let Some(on_request) = keg.on_request {
                            keg_json["installed_on_request"] = json!(on_request);
                        }
                        keg_json
                    })
                    .collect();
                let unread = self.spell_all(&f.unread);
                json!({
                    "name": short_name(k),
                    "full_name": self.full_name(k),
                    "tap": if f.tap { "user/tap" } else { "homebrew/core" },
                    "pinned": f.pinned,
                    "dependencies": self.spell_all(&f.declared),
                    "recommended_dependencies": self.spell_all(&f.recommended),
                    "build_dependencies": unread,
                    "optional_dependencies": unread,
                    "installed": kegs,
                })
            })
            .collect();
        let casks: Vec<Value> = self
            .casks
            .iter()
            .enumerate()
            .map(|(c, cask)| {
                let names = self.spell_all(&cask.formulae);
                let formula = match (cask.lone, names.first()) {
                    (true, Some(first)) => json!(first),
                    _ => json!(names),
                };
                json!({"token": format!("cask{c}"), "full_token": format!("cask{c}"), "depends_on": {"formula": formula}})
            })
            .collect();
        json!({"formulae": formulae, "casks": casks}).to_string()
    }
}

fn dep(n: usize) -> impl Strategy<Value = Dep> {
    let spelling = prop_oneof![Just(Spelling::Short), Just(Spelling::Full), Just(Spelling::OtherTap)];
    (0..n + GHOSTS, spelling).prop_map(|(target, spelling)| Dep { target, spelling })
}

fn deps(n: usize) -> impl Strategy<Value = Vec<Dep>> {
    prop::collection::vec(dep(n), 0..4)
}

fn keg(n: usize) -> impl Strategy<Value = Keg> {
    let on_request = prop_oneof![2 => Just(Some(false)), 1 => Just(Some(true)), 1 => Just(None)];
    let runtime = prop_oneof![4 => deps(n).prop_map(Some), 1 => Just(None)];
    (on_request, runtime).prop_map(|(on_request, runtime)| Keg { on_request, runtime })
}

fn formula(n: usize) -> impl Strategy<Value = Formula> {
    (any::<bool>(), prop::bool::weighted(0.15), prop::collection::vec(keg(n), 1..=3), deps(n), deps(n), deps(n))
        .prop_map(|(tap, pinned, kegs, declared, recommended, unread)| Formula {
            tap,
            pinned,
            kegs,
            declared,
            recommended,
            unread,
        })
}

fn cask(n: usize) -> impl Strategy<Value = Cask> {
    (prop::collection::vec(dep(n), 0..3), prop::bool::weighted(0.2))
        .prop_map(|(formulae, lone)| Cask { formulae, lone })
}

fn installs() -> impl Strategy<Value = Install> {
    (1..=MAX_FORMULAE).prop_flat_map(|n| {
        (prop::collection::vec(formula(n), n), prop::collection::vec(cask(n), 0..3))
            .prop_map(|(formulae, casks)| Install { formulae, casks })
    })
}

/// The install as the reference reads it, by index of the generated formula.
struct Reference {
    /// What each formula needs, among the installed ones: a keg's tab, or
    /// for a blank tab its declared and recommended dependencies, matched
    /// by the rack's name; never itself, never a build dependency.
    needs: Vec<BTreeSet<usize>>,
    on_request: Vec<bool>,
    pinned: Vec<bool>,
    /// A cask names it, by any spelling of its rack's name.
    cask_needed: Vec<bool>,
}

impl Reference {
    fn new(install: &Install) -> Self {
        let n = install.formulae.len();
        let installed = |d: &Dep| (d.target < n).then_some(d.target);
        let needs = install
            .formulae
            .iter()
            .enumerate()
            .map(|(k, f)| {
                let mut needs = BTreeSet::new();
                for keg in &f.kegs {
                    match &keg.runtime {
                        Some(deps) => needs.extend(deps.iter().filter_map(installed)),
                        None => needs.extend(f.declared.iter().chain(&f.recommended).filter_map(installed)),
                    }
                }
                needs.remove(&k);
                needs
            })
            .collect();
        let cask_needed = (0..n)
            .map(|k| {
                install.casks.iter().any(|c| {
                    let named = if c.lone { &c.formulae[..c.formulae.len().min(1)] } else { &c.formulae[..] };
                    named.iter().any(|d| d.target == k)
                })
            })
            .collect();
        Self {
            needs,
            on_request: install.formulae.iter().map(|f| f.kegs.iter().any(|k| k.on_request == Some(true))).collect(),
            pinned: install.formulae.iter().map(|f| f.pinned).collect(),
            cask_needed,
        }
    }

    fn len(&self) -> usize {
        self.needs.len()
    }

    /// Everything `i` needs, recursively, without `i`.
    fn closure(&self, i: usize) -> BTreeSet<usize> {
        let mut seen = BTreeSet::new();
        let mut todo = vec![i];
        while let Some(at) = todo.pop() {
            for &d in &self.needs[at] {
                if d != i && seen.insert(d) {
                    todo.push(d);
                }
            }
        }
        seen
    }

    /// The candidates nothing keeps once `removed` is gone: what stays is
    /// what a cask needs and whatever is reached through what is needed from
    /// a formula that stays (one not removed and not a candidate). The rest
    /// is the largest set that nothing outside it needs.
    fn left(&self, removed: Option<usize>, candidates: &BTreeSet<usize>) -> BTreeSet<usize> {
        let mut kept: BTreeSet<usize> = candidates.iter().copied().filter(|&c| self.cask_needed[c]).collect();
        let mut todo: Vec<usize> = (0..self.len()).filter(|k| Some(*k) != removed && !candidates.contains(k)).collect();
        todo.extend(kept.iter().copied());
        while let Some(at) = todo.pop() {
            for &d in &self.needs[at] {
                if candidates.contains(&d) && kept.insert(d) {
                    todo.push(d);
                }
            }
        }
        candidates - &kept
    }

    /// What is orphaned now.
    fn orphans(&self) -> BTreeSet<usize> {
        let candidates = (0..self.len()).filter(|&k| !self.on_request[k] && !self.pinned[k]).collect();
        self.left(None, &candidates)
    }

    /// What removing `i` orphans, and the pinned ones it would orphan.
    fn would_orphan(&self, i: usize) -> (BTreeSet<usize>, BTreeSet<usize>) {
        let candidates: BTreeSet<usize> = self.closure(i).into_iter().filter(|&d| !self.on_request[d]).collect();
        let unpinned = candidates.iter().copied().filter(|&d| !self.pinned[d]).collect();
        let kept_pinned = self.left(Some(i), &candidates).into_iter().filter(|&d| self.pinned[d]).collect();
        (self.left(Some(i), &unpinned), kept_pinned)
    }

    /// Who needs `s` among the formulae that stay when `leaving` goes.
    fn remaining_needers(&self, s: usize, leaving: &BTreeSet<usize>) -> Vec<usize> {
        (0..self.len()).filter(|j| !leaving.contains(j) && self.needs[*j].contains(&s)).collect()
    }
}

/// The graph's indices of the generated formulae, and back.
fn indices(g: &Graph, install: &Install) -> (Vec<usize>, BTreeMap<usize, usize>) {
    let to_graph: Vec<usize> = (0..install.formulae.len())
        .map(|k| g.find(&install.full_name(k)).unwrap_or_else(|| panic!("{} is listed", install.full_name(k))))
        .collect();
    let back = to_graph.iter().enumerate().map(|(k, &i)| (i, k)).collect();
    (to_graph, back)
}

fn ours(back: &BTreeMap<usize, usize>, set: &BTreeSet<usize>) -> BTreeSet<usize> {
    set.iter().map(|i| back[i]).collect()
}

fn theirs(to_graph: &[usize], set: &BTreeSet<usize>) -> BTreeSet<usize> {
    set.iter().map(|&k| to_graph[k]).collect()
}

/// `set` may go with `removed`: no member is on request or pinned, and no
/// formula that stays and no cask needs one, by the reference's reading
/// and by [`Graph::needed_by`], which the re-read before a removal asks.
fn assert_may_go(
    r: &Reference,
    g: &Graph,
    to_graph: &[usize],
    removed: Option<usize>,
    set: &BTreeSet<usize>,
) -> Result<(), TestCaseError> {
    let mut leaving = set.clone();
    leaving.extend(removed);
    let graph_leaving = theirs(to_graph, &leaving);
    for &s in set {
        prop_assert!(Some(s) != removed, "f{s} is the removal itself");
        prop_assert!(!r.on_request[s] && !r.pinned[s], "f{s} is on request or pinned");
        prop_assert!(!r.cask_needed[s], "a cask needs f{s}");
        let needers = r.remaining_needers(s, &leaving);
        prop_assert!(needers.is_empty(), "f{s} is needed by {needers:?}, which stay");
        let needed = g.needed_by(to_graph[s], &graph_leaving);
        prop_assert!(needed.is_empty(), "the graph says {needed:?}");
    }
    Ok(())
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn an_orphan_set_never_names_a_formula_a_remaining_formula_or_a_cask_needs(install in installs()) {
        let json = install.json();
        let g = Graph::new(&info::parse(&json).expect("brew info parses"));
        prop_assert!(g.incomplete.is_empty(), "{:?}", g.incomplete);
        prop_assert_eq!(g.nodes().len(), install.formulae.len());
        let r = Reference::new(&install);
        let (to_graph, back) = indices(&g, &install);

        // What is orphaned now: safe to remove together, and all of it.
        let now = ours(&back, &g.orphans());
        assert_may_go(&r, &g, &to_graph, None, &now)?;
        prop_assert_eq!(&now, &r.orphans(), "orphans now; brew info: {}", json);

        for k in 0..install.formulae.len() {
            let i = to_graph[k];
            let orphaned = g.would_orphan(i);
            let (orphans, kept_pinned) = (ours(&back, &orphaned.orphans), ours(&back, &orphaned.kept_pinned));
            // Only its dependencies, and none a remaining formula or a cask needs.
            prop_assert!(orphans.is_subset(&r.closure(k)), "f{} does not need all of {:?}", k, orphans);
            assert_may_go(&r, &g, &to_graph, Some(k), &orphans)?;
            prop_assert!(kept_pinned.iter().all(|&d| r.pinned[d]) && kept_pinned.is_disjoint(&orphans));
            let (want, want_pinned) = r.would_orphan(k);
            prop_assert_eq!(&orphans, &want, "what removing f{} orphans; brew info: {}", k, json);
            prop_assert_eq!(&kept_pinned, &want_pinned, "the pinned ones of f{}; brew info: {}", k, json);
            let closure = ours(&back, &g.closure(i));
            prop_assert_eq!(&closure, &r.closure(k), "what f{} needs; brew info: {}", k, json);
        }
    }
}
