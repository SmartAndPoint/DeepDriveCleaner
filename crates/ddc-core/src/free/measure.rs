//! The allocated bytes of the folders an area action frees, measured
//! before and after it: ddc never reports space it did not free.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::scan::{self, NullVisitor, Progress, WalkOptions};

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

/// The sum of [`allocated`] over `paths`; `None` when any is not measured.
pub(crate) fn allocated_all(paths: &[PathBuf], skips: &[PathBuf]) -> Option<u64> {
    paths.iter().map(|p| allocated(p, skips)).sum()
}
