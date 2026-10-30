//! The `/opt` conda installs of the disk report (story "Whole disk: Conda
//! environments and its package cache, with conda"). Environments come
//! from the file system, never from conda, which this module never runs.
//! An install's environments and its package folder are the regions of one
//! walk: a file linked in an environment and in `pkgs` counts in both
//! `shared_bytes`, never in what removing the environment frees.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::model::{Conda, CondaFolder, CondaInstall};
use super::visitor::DiskVisitor;
use crate::platform::fsops::can_write;
use crate::rules::Catalog;
use crate::scan::{self, Progress, Tag, WalkOptions};

/// Each `/opt` install under `opt` (the firmlinked `/opt`), its environments
/// and its package folder measured; `None` when there is none.
pub(super) fn installs(
    opt: &Path,
    catalog: &Catalog,
    skips: &[PathBuf],
    threads: Option<usize>,
    cancel: &AtomicBool,
) -> Option<Conda> {
    let prefixes: Vec<PathBuf> = crate::conda::PREFIXES.iter().map(|p| opt.join(p)).filter(|p| p.is_dir()).collect();
    if prefixes.is_empty() {
        return None;
    }
    // Per install, its environments then its `pkgs`; each folder a region,
    // walked by its canonical path and reported through the firmlink.
    let mut layout: Vec<(PathBuf, Vec<PathBuf>, Option<PathBuf>)> = Vec::new();
    let mut regions: Vec<(PathBuf, Tag)> = Vec::new();
    let mut tags: Vec<Option<Tag>> = Vec::new();
    let mut add = |folder: &Path, regions: &mut Vec<(PathBuf, Tag)>| {
        let tag = folder.canonicalize().ok().map(|canon| {
            let tag = regions.len() as Tag;
            regions.push((canon, tag));
            tag
        });
        tags.push(tag);
    };
    for prefix in prefixes {
        let envs = crate::conda::environments(&prefix);
        let pkgs = Some(prefix.join("pkgs")).filter(|p| p.is_dir());
        for folder in envs.iter().chain(&pkgs) {
            add(folder, &mut regions);
        }
        layout.push((prefix, envs, pkgs));
    }

    let roots: Vec<PathBuf> = regions.iter().map(|(p, _)| p.clone()).collect();
    let visitor = DiskVisitor::new(catalog, skips, &regions, vec![false; regions.len()], u64::MAX, cancel);
    let opts = WalkOptions { threads, ..WalkOptions::default() };
    let out = if roots.is_empty() || cancel.load(Ordering::SeqCst) {
        None
    } else {
        scan::walk(&roots, &visitor, &opts, &Progress::default()).ok()
    };

    let mut tags = tags.into_iter();
    let mut measured = |path: &Path, environment: bool| -> CondaFolder {
        let tag = tags.next().flatten();
        let owner = tag.and_then(|t| out.iter().flat_map(|o| &o.owners).find(|o| o.tag == t));
        let pruned = tag.is_some_and(|t| visitor.pruned[t as usize].load(Ordering::Relaxed) > 0);
        let writable = can_write(path) && (!environment || path.parent().is_some_and(can_write));
        CondaFolder {
            path: path.to_path_buf(),
            bytes: owner.map_or(0, |o| o.bytes),
            shared_bytes: owner.map_or(0, |o| o.shared_bytes),
            files: owner.map_or(0, |o| o.files),
            unreadable: owner.map_or(0, |o| o.errors),
            partial: pruned || owner.is_none(),
            writable,
        }
    };
    let installs = layout
        .into_iter()
        .map(|(prefix, envs, pkgs)| {
            let environments = envs.iter().map(|e| measured(e, true)).collect();
            let packages = pkgs.as_deref().map(|p| measured(p, false));
            CondaInstall { prefix, packages, environments }
        })
        .collect();
    Some(Conda { installs })
}
