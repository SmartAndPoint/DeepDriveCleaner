//! The clean's re-check of an explicit list (story "Faster clean: re-check
//! only what the selection needs"; safety ADR "Cleanup safety model",
//! amendment of 2026-10-24 and its note of 2026-10-25). A list whose rules
//! need the whole tree, or a run with no report to edit after the clean,
//! takes the full walk. Any other list takes the same scan pipeline over a
//! walk pruned to the listed items, their ancestors, and the siblings of a
//! [`Scope::Siblings`] item: one classification path, the pruned walk only
//! leaves directories unread, inside claims too.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::contents::EntryIndex;
use super::volumes::StatFs;
use super::{ScanError, ScanOptions};
use crate::env::Env;
use crate::model::ScanReport;
use crate::plan::ListedItem;
use crate::rules::{Catalog, Scope};
use crate::scan::{
    DirCtx, DirListing, DirReader, EntryMeta, OwnerKind, Post, Pre, Progress, ReadErrorKind, Tag, Visitor,
};

/// Which walk a clean's re-check takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Walk {
    Pruned,
    Full(Full),
}

/// Why a re-check walks the whole tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Full {
    /// A listed rule's verdict reads files anywhere in the scan roots.
    Tree,
    /// No readable report of the current schema, of the clean's roots and
    /// home, to edit after the clean ([`same_roots`]).
    NoReport,
    /// An ancestor of a listed item could not be read in the pruned walk.
    UnreadableAncestor,
}

/// The walk `list` needs, given the report on disk. A rule the catalog lacks
/// needs none: the plan refuses its item as `rule-mismatch`.
pub fn walk_for(list: &[ListedItem], catalog: &Catalog, previous: Option<&ScanReport>) -> Walk {
    if previous.is_none() {
        return Walk::Full(Full::NoReport);
    }
    for rule in list.iter().filter_map(|item| catalog.by_id(&item.rule)) {
        match rule.scope {
            Scope::Tree => return Walk::Full(Full::Tree),
            Scope::Item | Scope::Siblings => {}
        }
    }
    Walk::Pruned
}

/// Whether `report` was scanned from the roots and the home this clean
/// walks, normalized as the walk normalizes them (gate decision 10a). A
/// report of `ddc scan ~/Code` is not the one to edit after a clean of the
/// home: the caller takes it for no report.
pub fn same_roots(report: &ScanReport, env: &Env, opts: &ScanOptions) -> bool {
    report.home == env.home()
        && crate::scan::normalized_roots(&super::roots_of(env, opts)).is_ok_and(|roots| roots == report.scan_roots)
}

/// The directories a pruned re-check opens.
#[derive(Debug, Default)]
pub struct Reach {
    /// Listed paths, with their whole subtrees.
    items: HashSet<PathBuf>,
    /// Every ancestor of a listed path: read, with the rules applied, and
    /// nothing below it that is not otherwise reached.
    ancestors: HashSet<PathBuf>,
    /// Parents of `Siblings` items, with their whole subtrees.
    families: HashSet<PathBuf>,
}

impl Reach {
    pub fn new(list: &[ListedItem], catalog: &Catalog) -> Self {
        let mut reach = Reach::default();
        for item in list {
            let Some(rule) = catalog.by_id(&item.rule) else { continue };
            match rule.scope {
                Scope::Siblings => {
                    if let Some(parent) = item.path.parent() {
                        reach.families.insert(parent.to_path_buf());
                    }
                }
                // A list with a `Tree` item never takes the pruned walk.
                Scope::Item | Scope::Tree => {}
            }
            reach.ancestors.extend(item.path.ancestors().skip(1).map(Path::to_path_buf));
            reach.items.insert(item.path.clone());
        }
        reach
    }

    /// Whether the re-check opens `dir`: a few lookups per level of `dir`,
    /// whatever the length of the list.
    pub fn allows(&self, dir: &Path) -> bool {
        self.ancestors.contains(dir)
            || dir.ancestors().any(|a| self.items.contains(a) || (a != dir && self.families.contains(a)))
    }

    /// Whether `dir` lies above a listed item.
    fn above_an_item(&self, dir: &Path) -> bool {
        self.ancestors.contains(dir)
    }
}

/// The rule visitor, with directories outside the reach left unread.
pub(crate) struct Pruned<'a, V> {
    pub(crate) inner: &'a V,
    pub(crate) reach: &'a Reach,
}

impl<V: Visitor> Visitor for Pruned<'_, V> {
    fn before_read(&self, dir: &DirCtx<'_>) -> Pre {
        self.inner.before_read(dir)
    }

    fn after_read(&self, dir: &DirCtx<'_>, listing: &DirListing) -> Post {
        self.inner.after_read(dir, listing)
    }

    fn wants_private(&self, dir: &DirCtx<'_>, tag: Tag) -> bool {
        self.inner.wants_private(dir, tag)
    }

    fn lists_entries(&self, dir: &DirCtx<'_>, tag: Tag) -> bool {
        self.inner.lists_entries(dir, tag)
    }

    fn on_file(&self, dir: &Path, entry: &EntryMeta, owner: Option<(OwnerKind, Tag)>) {
        self.inner.on_file(dir, entry, owner);
    }

    fn prunes(&self, dir: &Path) -> bool {
        !self.reach.allows(dir)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RecheckError {
    #[error(transparent)]
    Scan(#[from] ScanError),
    /// The caller takes the full walk instead (safety ADR, amendment of 2026-10-24).
    #[error("cannot read {0}, above a listed item")]
    UnreadableAncestor(PathBuf),
}

/// The re-check over the pruned walk: for the listed items, the report and
/// entry index the full walk gives. The report's other figures are partial
/// and never saved: after the clean the CLI edits the report it read.
/// `reader` replaces the one `opts.walk` names, so a test can count the
/// directories opened.
pub fn pruned(
    env: &Env,
    opts: &ScanOptions,
    catalog: &Catalog,
    list: &[ListedItem],
    progress: &Progress,
    statfs: &dyn StatFs,
    reader: Option<&dyn DirReader>,
) -> Result<(ScanReport, EntryIndex), RecheckError> {
    let reach = Reach::new(list, catalog);
    let scanned = super::scan_inner(env, opts, progress, statfs, reader, Some(&reach))?;
    let unreadable = scanned.errors.iter().find(|e| {
        matches!(e.error.kind, ReadErrorKind::PermissionDenied | ReadErrorKind::Other) && reach.above_an_item(&e.path)
    });
    if let Some(e) = unreadable {
        return Err(RecheckError::UnreadableAncestor(e.path.clone()));
    }
    Ok((scanned.report, scanned.index))
}

/// The pruned re-check's estimate of the files it reads: the listed
/// findings' files and, for a `Siblings` item, its sibling findings'. A
/// picked entry counts none. `None` when that is nothing.
pub fn expected_files(previous: &ScanReport, list: &[ListedItem], catalog: &Catalog) -> Option<u64> {
    let reach = Reach::new(list, catalog);
    let files = previous
        .findings
        .iter()
        .filter(|f| reach.items.contains(&f.path) || f.path.parent().is_some_and(|p| reach.families.contains(p)))
        .map(|f| f.files)
        .sum();
    (files > 0).then_some(files)
}
