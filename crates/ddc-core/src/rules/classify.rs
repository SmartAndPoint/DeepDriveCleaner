//! After the walk: effective class of every finding (ADR §3), keep markers,
//! protected toolchain versions, unlisted rules.

use std::fs;
use std::path::{Path, PathBuf};

use super::matcher::glob_match;
use super::{KEPT, Rule, RuleVisitor, toolchain};
use crate::class::{Category, RiskClass};
use crate::scan::{OwnerKind, OwnerResult, WalkOutput};

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
    let dropped = toolchain::dropped(&out.owners, catalog, &visitor.pins());

    let mut found = Vec::new();
    for owner in &out.owners {
        let mut rule = catalog.rule(owner.tag);
        if !rule.listed || dropped.contains(&owner.path) {
            continue;
        }
        let mut notes = Vec::new();
        let excluded_inside = visitor.excludes().iter().any(|e| e.starts_with(&owner.path));
        if rule.id == KEPT {
            notes.push(if excluded_inside || visitor.excludes().iter().any(|e| owner.path.starts_with(e)) {
                Note::Excluded
            } else {
                Note::KeptByMarker
            });
        } else if owner.kind == OwnerKind::Claim && excluded_inside {
            // `--exclude` below a finding protects the whole finding.
            rule = kept;
            notes.push(Note::Excluded);
        } else if owner.kind == OwnerKind::Claim && markers.iter().any(|m| m.starts_with(&owner.path)) {
            rule = kept;
            notes.push(Note::KeptByMarker);
        }
        let mut class = rule.class;
        let project_root = (rule.category == Category::ProjectArtifact && rule.id != KEPT)
            .then(|| project_root(&owner.path, &out.roots.iter().map(|r| r.path.as_path()).collect::<Vec<_>>(), home));
        let has_lockfile = rule.lockfiles.map(|locks| {
            let parent = owner.path.parent();
            [parent, project_root.as_deref()].into_iter().flatten().any(|dir| dir_has_any(dir, locks))
        });
        if class == RiskClass::Rebuildable && app_owned(&owner.path, home) {
            class = RiskClass::Review;
            notes.push(Note::AppOwned);
        }
        if class == RiskClass::Rebuildable && has_lockfile == Some(false) {
            class = RiskClass::Review;
            notes.push(Note::NoLockfile);
        }
        found.push(Classified { rule, owner: owner.clone(), class, notes, project_root, has_lockfile });
    }
    found
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
    fs::read_dir(dir).is_ok_and(|rd| {
        rd.filter_map(Result::ok)
            .any(|e| e.file_name().to_str().is_some_and(|n| patterns.iter().any(|p| glob_match(p, n))))
    })
}
