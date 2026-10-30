//! The visitor of the whole-disk walk: it only measures. It never claims a
//! folder, never classifies and creates no findings; each area is a region,
//! so a nested area's bytes leave its parent's.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

use crate::rules::Catalog;
use crate::scan::{DirCtx, DirListing, Post, Pre, SkipReason, Tag, Visitor};

/// Entries one directory of a budgeted area may hold and still be read.
const LISTING_LIMIT: u64 = 10_000;

/// A directory's entries from its `lstat`. On APFS the link count is the
/// entries plus two, up to 65,535, where it stops; past it, the size is 32
/// bytes per entry, plus two. Elsewhere the link count is lower (HFS+ counts
/// subdirectories only), so the directory is read: this fails open.
fn entries_of(nlink: u64, size: u64) -> u64 {
    let linked = nlink.saturating_sub(2);
    if nlink >= u64::from(u16::MAX) { linked.max((size / 32).saturating_sub(2)) } else { linked }
}

pub(crate) struct DiskVisitor<'a> {
    pub catalog: &'a Catalog,
    /// Canonical: never opened, at or below.
    pub user_skips: &'a [PathBuf],
    /// Canonical area path → its tag.
    pub regions: HashMap<PathBuf, Tag>,
    /// The same paths, deepest first, for the area a directory lies in.
    pub by_depth: Vec<(PathBuf, Tag)>,
    /// Per tag: walked under the entry budget.
    pub budgeted: Vec<bool>,
    pub budget: u64,
    /// Per tag: entries read.
    pub entries: Vec<AtomicU64>,
    /// Per tag: directories left unread (budget or stop).
    pub pruned: Vec<AtomicU64>,
    /// Some budgeted area reached its budget.
    pub exhausted: AtomicBool,
    pub cancel: &'a AtomicBool,
    /// The user's own home folder seen through the Data volume: the home
    /// row counts it.
    pub hide: Option<PathBuf>,
    /// The canonical Data root: a skip or a cloud folder is named through
    /// `/` (`/opt/x`), the walk reaches it through the Data volume.
    pub data_root: Option<PathBuf>,
}

impl DiskVisitor<'_> {
    pub fn new<'a>(
        catalog: &'a Catalog,
        user_skips: &'a [PathBuf],
        areas: &[(PathBuf, Tag)],
        budgeted: Vec<bool>,
        budget: u64,
        cancel: &'a AtomicBool,
    ) -> DiskVisitor<'a> {
        let mut regions = HashMap::new();
        for (path, tag) in areas {
            regions.entry(path.clone()).or_insert(*tag);
        }
        let mut by_depth: Vec<(PathBuf, Tag)> = regions.iter().map(|(p, t)| (p.clone(), *t)).collect();
        by_depth.sort_by_key(|(p, _)| std::cmp::Reverse(p.components().count()));
        let n = budgeted.len();
        DiskVisitor {
            catalog,
            user_skips,
            regions,
            by_depth,
            budgeted,
            budget,
            entries: (0..n).map(|_| AtomicU64::new(0)).collect(),
            pruned: (0..n).map(|_| AtomicU64::new(0)).collect(),
            exhausted: AtomicBool::new(false),
            cancel,
            hide: None,
            data_root: None,
        }
    }

    /// Never opened: a skip or a cloud folder, by the walked path or by the
    /// same path without the Data root.
    fn never_open(&self, dir: &Path) -> bool {
        self.catalog.never_open(dir, self.user_skips).is_some()
            || self
                .data_root
                .as_deref()
                .and_then(|root| dir.strip_prefix(root).ok())
                .is_some_and(|rel| self.catalog.never_open(&Path::new("/").join(rel), self.user_skips).is_some())
    }

    /// A directory of a budgeted area with more entries than the budget has
    /// left, or than one listing may hold. Reading one listing of 800,000
    /// entries took 300 s before a single entry counted, so it is not opened.
    fn too_big(&self, dir: &Path, tag: Tag) -> bool {
        let i = tag as usize;
        if !self.budgeted.get(i).copied().unwrap_or(false) {
            return false;
        }
        let left = self.budget.saturating_sub(self.entries[i].load(Relaxed)).min(LISTING_LIMIT);
        std::fs::symlink_metadata(dir)
            .is_ok_and(|md| entries_of(crate::platform::meta::link_count(&md), md.len()) > left)
    }

    fn area_of(&self, dir: &Path) -> Option<Tag> {
        self.by_depth.iter().find(|(p, _)| dir.starts_with(p)).map(|(_, t)| *t)
    }
}

impl Visitor for DiskVisitor<'_> {
    fn before_read(&self, dir: &DirCtx<'_>) -> Pre {
        if self.never_open(dir.path) {
            return Pre::Skip(SkipReason::Excluded);
        }
        let region = self.regions.get(dir.path).copied();
        if let Some(tag) = region.or(dir.owner.map(|(_, t)| t))
            && self.too_big(dir.path, tag)
        {
            self.pruned[tag as usize].fetch_add(1, Relaxed);
            return Pre::Skip(SkipReason::Excluded);
        }
        match region {
            Some(tag) => Pre::Region(tag),
            None => Pre::Continue,
        }
    }

    fn after_read(&self, dir: &DirCtx<'_>, listing: &DirListing) -> Post {
        if let Some((_, tag)) = dir.owner
            && self.budgeted.get(tag as usize).copied().unwrap_or(false)
        {
            let read = self.entries[tag as usize].fetch_add(listing.entries.len() as u64, Relaxed)
                + listing.entries.len() as u64;
            if read >= self.budget {
                self.exhausted.store(true, Relaxed);
            }
        }
        Post::Descend
    }

    fn wants_private(&self, _: &DirCtx<'_>, _: Tag) -> bool {
        false
    }

    fn prunes(&self, dir: &Path) -> bool {
        if self.hide.as_deref() == Some(dir) {
            return true;
        }
        let cancelled = self.cancel.load(Relaxed);
        if !cancelled && !self.exhausted.load(Relaxed) {
            return false;
        }
        let Some(tag) = self.area_of(dir) else { return cancelled };
        let i = tag as usize;
        if cancelled || (self.budgeted[i] && self.entries[i].load(Relaxed) >= self.budget) {
            self.pruned[i].fetch_add(1, Relaxed);
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::entries_of;

    #[test]
    fn entries_come_from_the_link_count_and_past_its_cap_from_the_size() {
        assert_eq!(entries_of(2, 64), 0);
        assert_eq!(entries_of(1002, 32_064), 1000);
        // `$TMPDIR` on the reference machine, 2026-10-28.
        assert_eq!(entries_of(65_535, 26_841_120), 838_783);
        // Not APFS: a small link count is taken as it is.
        assert_eq!(entries_of(5, 1_000_000), 3);
    }
}
