//! After the walk: effective class of every finding (ADR §3), keep markers,
//! protected toolchain versions, unlisted rules.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::entries::ENTRY_RULES;
use super::matcher::glob_match;
use super::{KEPT, Rule, RuleVisitor, toolchain};
use crate::class::{Category, RiskClass};
use crate::scan::{OwnerKind, OwnerResult, WalkOutput};
use crate::stats::{self, Counter};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum Note {
    /// Environment without a lock file: restoring may give a different environment.
    NoLockfile,
    /// Inside a hidden top-level folder of $HOME or ~/Library: owned by an app or tool.
    AppOwned,
    /// A `.ddc-keep` marker in it or above it.
    KeptByMarker,
    /// Matched `--exclude`.
    Excluded,
}

#[derive(Debug, Clone)]
pub struct Classified {
    pub rule: &'static Rule,
    pub owner: OwnerResult,
    /// Effective class after the checks below; may be lower than `rule.class`.
    pub class: RiskClass,
    pub notes: Vec<Note>,
    /// Project artifacts: nearest ancestor with `.git` (dir or worktree file)
    /// within the scan root, else the artifact's parent.
    pub project_root: Option<PathBuf>,
    /// Lock-gated rules: whether a lock file was found.
    pub has_lockfile: Option<bool>,
}

pub fn classify(out: &WalkOutput, visitor: &RuleVisitor<'_>) -> Vec<Classified> {
    let catalog = visitor.catalog();
    let home = catalog.home();
    let kept = catalog.by_id(KEPT).expect("catalog defines user.kept");
    let markers = visitor.keep_markers();
    let marked = AtOrAbove::new(markers.iter().map(PathBuf::as_path));
    let mut locks = LockFiles::default();
    let dropped = toolchain::dropped(&out.owners, catalog, &visitor.pins());

    let mut found = Vec::new();
    for owner in &out.owners {
        let mut rule = catalog.rule(owner.tag);
        if !rule.listed || dropped.contains(&owner.path) {
            continue;
        }
        let mut notes = Vec::new();
        let excluded_inside =
            visitor.excludes().iter().any(|e| stats::counted(Counter::AncestorChecks, || e.starts_with(&owner.path)));
        if rule.id == KEPT {
            notes.push(
                if excluded_inside
                    || visitor
                        .excludes()
                        .iter()
                        .any(|e| stats::counted(Counter::AncestorChecks, || owner.path.starts_with(e)))
                {
                    Note::Excluded
                } else {
                    Note::KeptByMarker
                },
            );
        } else if owner.kind == OwnerKind::Claim && excluded_inside {
            // `--exclude` below a finding protects the whole finding.
            rule = kept;
            notes.push(Note::Excluded);
        } else if owner.kind == OwnerKind::Claim && !ENTRY_RULES.contains(&rule.id) && marked.contains(&owner.path) {
            // In the user's own folders a marker locks the entry that holds it
            // instead (safety ADR, amendment of 2026-10-20); one at the folder's
            // top made it `user.kept` before it was read.
            rule = kept;
            notes.push(Note::KeptByMarker);
        }
        let roots: Vec<&Path> = out.roots.iter().map(|r| r.path.as_path()).collect();
        let (class, project_root, has_lockfile) = downgraded(rule, &owner.path, &roots, home, &mut locks, &mut notes);
        found.push(Classified { rule, owner: owner.clone(), class, notes, project_root, has_lockfile });
    }
    found
}

/// The class `classify` gives a finding of `rule` at `path` without a walk:
/// the app-owned and lock-file downgrades. Pins, ancestor claims and keep
/// markers need the walk (story "CLI: clean an explicit item list": the dry run).
/// `locks` is shared by the items of one list.
pub fn effective_class(rule: &Rule, path: &Path, roots: &[&Path], home: &Path, locks: &mut LockFiles) -> RiskClass {
    downgraded(rule, path, roots, home, locks, &mut Vec::new()).0
}

/// Whether a folder holds a file of a lock list, read once per folder and
/// list: every environment of one project asks about the same project root,
/// and a root of many projects is not listed once per environment (story
/// "Complexity guard: operation counts and scaling exponents").
#[derive(Debug, Default)]
pub struct LockFiles(HashMap<&'static [&'static str], HashMap<PathBuf, bool>>);

impl LockFiles {
    fn any(&mut self, dir: &Path, patterns: &'static [&'static str]) -> bool {
        let seen = self.0.entry(patterns).or_default();
        if let Some(&found) = seen.get(dir) {
            return found;
        }
        let found = dir_has_any(dir, patterns);
        seen.insert(dir.to_path_buf(), found);
        found
    }
}

/// Every folder at or above one of a set of paths, built once:
/// `contains(dir)` is `paths.any(|p| p.starts_with(dir))` in one lookup
/// instead of a scan of the paths per question.
pub(crate) struct AtOrAbove(HashSet<PathBuf>);

impl AtOrAbove {
    pub(crate) fn new<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Self {
        let mut set = HashSet::new();
        for path in paths {
            for dir in path.ancestors() {
                stats::add(Counter::MarkerChecks, 1);
                // Its own ancestors are in already.
                if !set.insert(dir.to_path_buf()) {
                    break;
                }
            }
        }
        Self(set)
    }

    pub(crate) fn contains(&self, dir: &Path) -> bool {
        stats::counted(Counter::MarkerChecks, || self.0.contains(dir))
    }
}

/// The rule's class after the downgrades, the project root and whether a
/// lock file was found; the notes explain each downgrade.
fn downgraded(
    rule: &Rule,
    path: &Path,
    roots: &[&Path],
    home: &Path,
    locks: &mut LockFiles,
    notes: &mut Vec<Note>,
) -> (RiskClass, Option<PathBuf>, Option<bool>) {
    let mut class = rule.class;
    let project_root =
        (rule.category == Category::ProjectArtifact && rule.id != KEPT).then(|| project_root(path, roots, home));
    let has_lockfile = rule.lockfiles.map(|patterns| {
        let parent = path.parent();
        [parent, project_root.as_deref()].into_iter().flatten().any(|dir| locks.any(dir, patterns))
    });
    if class == RiskClass::Rebuildable && app_owned(path, home) {
        class = RiskClass::Review;
        notes.push(Note::AppOwned);
    }
    if class == RiskClass::Rebuildable && has_lockfile == Some(false) {
        class = RiskClass::Review;
        notes.push(Note::NoLockfile);
    }
    (class, project_root, has_lockfile)
}

/// Nearest ancestor of the artifact's parent with `.git` (directory or
/// worktree file), not above the scan root; otherwise the parent itself.
/// A `.git` directly in `$HOME` (a dotfiles repository) is not a project root.
pub(crate) fn project_root(artifact: &Path, roots: &[&Path], home: &Path) -> PathBuf {
    let parent = artifact.parent().unwrap_or(artifact);
    let Some(limit) = roots.iter().filter(|r| artifact.starts_with(r)).max_by_key(|r| r.as_os_str().len()).copied()
    else {
        return parent.to_path_buf();
    };
    if !parent.starts_with(limit) {
        // The artifact is the scan root itself: never look above it.
        return parent.to_path_buf();
    }
    for dir in parent.ancestors() {
        stats::add(Counter::AncestorChecks, 1);
        if dir != home && fs::symlink_metadata(dir.join(".git")).is_ok() {
            return dir.to_path_buf();
        }
        if dir == limit {
            break;
        }
    }
    parent.to_path_buf()
}

/// Hidden top-level folder of $HOME (`~/.vscode`, `~/.local`, …) or `~/Library`.
pub(crate) fn app_owned(path: &Path, home: &Path) -> bool {
    path.strip_prefix(home).ok().and_then(|rel| rel.components().next()).is_some_and(|first| {
        let first = first.as_os_str().to_string_lossy();
        first.starts_with('.') || first == "Library"
    })
}

fn dir_has_any(dir: &Path, patterns: &[&str]) -> bool {
    if patterns.iter().all(|p| !p.contains('*')) {
        return patterns.iter().any(|p| fs::symlink_metadata(dir.join(p)).is_ok());
    }
    crate::stats::read_dir(dir).is_ok_and(|rd| {
        rd.filter_map(Result::ok)
            .any(|e| e.file_name().to_str().is_some_and(|n| patterns.iter().any(|p| glob_match(p, n))))
    })
}
