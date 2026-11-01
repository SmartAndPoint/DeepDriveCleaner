//! The allocated bytes of the folders an area action frees, measured
//! before and after it: ddc never reports space it did not free.

use crate::platform::Canonical as _;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::scan::{self, DirCtx, NullVisitor, Pre, Progress, Tag, Visitor, WalkOptions};

/// Allocated bytes under `path`, each inode once; `Some(0)` when it does
/// not exist. `None` when a skip of `settings.json` lies at, above or under
/// it: a skipped folder is never opened.
pub(crate) fn allocated(path: &Path, skips: &[PathBuf]) -> Option<u64> {
    if skips.iter().any(|s| path.starts_with(s) || s.starts_with(path)) {
        return None;
    }
    match std::fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => return Some(0),
        Err(_) => return None,
    }
    let out = scan::walk(&[path.to_path_buf()], &NullVisitor, &WalkOptions::default(), &Progress::default()).ok()?;
    Some(out.total_bytes)
}

/// The allocated bytes of the files under `path` whose every link lies
/// inside it: what removing that folder alone frees. A file hard-linked
/// from a package folder or from another environment is not counted, and
/// no other folder needs to be read to know it. `Some(0)` when it does not
/// exist, `None` as [`allocated`].
pub(crate) fn own(path: &Path, skips: &[PathBuf]) -> Option<u64> {
    if skips.iter().any(|s| path.starts_with(s) || s.starts_with(path)) {
        return None;
    }
    match std::fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => return Some(0),
        Err(_) => return None,
    }
    let out = scan::walk(&[path.to_path_buf()], &Alone, &WalkOptions::default(), &Progress::default()).ok()?;
    Some(out.owners.iter().map(|o| o.bytes).sum())
}

/// The walk's root is one region: its owner's `bytes` are its files whose
/// links all lie inside it.
struct Alone;

impl Visitor for Alone {
    fn before_read(&self, dir: &DirCtx<'_>) -> Pre {
        if dir.depth == 0 { Pre::Region(0) } else { Pre::Continue }
    }

    fn wants_private(&self, _: &DirCtx<'_>, _: Tag) -> bool {
        false
    }
}

/// [`allocated`] of each of `paths`, in one walk: an inode two of them
/// share is counted under the first that reaches it. `Some(0)` for one
/// that does not exist; `None` for one under a skip, one that cannot be
/// read, or one inside another of them. A skip is compared with each path
/// as given and with every link in it resolved: brew spells the Cellar
/// through `/opt`, the skips are canonical.
pub(crate) fn regions(paths: &[PathBuf], skips: &[PathBuf]) -> Vec<Option<u64>> {
    let skipped = |p: &Path| skips.iter().any(|s| p.starts_with(s) || s.starts_with(p));
    let canonical: Vec<Option<Option<PathBuf>>> = paths
        .iter()
        .map(|path| {
            if skipped(path) {
                return None;
            }
            match std::fs::symlink_metadata(path) {
                Ok(_) => path.canonical().ok().filter(|c| !skipped(c)).map(Some),
                Err(e) if e.kind() == ErrorKind::NotFound => Some(None),
                Err(_) => None,
            }
        })
        .collect();
    let roots: Vec<PathBuf> = canonical.iter().flatten().flatten().cloned().collect();
    let walked = if roots.is_empty() {
        None
    } else {
        scan::walk(&roots, &NullVisitor, &WalkOptions::default(), &Progress::default()).ok()
    };
    canonical
        .into_iter()
        .map(|c| match c? {
            None => Some(0),
            Some(root) => walked.as_ref()?.roots.iter().find(|r| r.path == root).map(|r| r.bytes),
        })
        .collect()
}

/// The sum of [`allocated`] over `paths`; `None` when any is not measured.
pub(crate) fn allocated_all(paths: &[PathBuf], skips: &[PathBuf]) -> Option<u64> {
    paths.iter().map(|p| allocated(p, skips)).sum()
}
