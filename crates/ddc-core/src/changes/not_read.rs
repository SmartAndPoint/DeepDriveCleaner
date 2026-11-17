//! What a report did not read: its `not_scanned` folders, its `skipped`
//! paths and its `inaccessible` ones, each with the kind that names it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::{NotRead, NotReadKind};
use crate::model::ScanReport;
use crate::rules::Matcher;
use crate::scan::SkipReason;

/// The cloud folders of `env`'s catalog, never read: the `user.cloud`
/// rule's, under the home, and the roots the environment names.
pub fn cloud_roots(env: &crate::env::Env) -> Vec<PathBuf> {
    let mut roots = match crate::rules::RULES.iter().find(|r| r.id == "user.cloud").map(|r| &r.matcher) {
        Some(Matcher::NoTraverse(paths)) => paths.iter().map(|p| env.home().join(p)).collect(),
        _ => Vec::new(),
    };
    roots.extend(env.cloud().iter().cloned());
    roots
}

/// Every path `report` did not read, once each, in the report's order:
/// `not_scanned`, then `skipped`, then `inaccessible`; a path listed twice
/// (a person's skip is in `not_scanned` and in `skipped`) keeps its first
/// kind, a cloud folder's being `cloud`. A path skipped as `holds-subvolume`
/// is not among them: the walk entered and counted it.
pub fn not_read(report: &ScanReport, cloud: &[PathBuf]) -> Vec<NotRead> {
    let in_cloud = |p: &Path| cloud.iter().any(|c| p.starts_with(c));
    let skip = |p: &Path| report.not_scanned.iter().any(|n| p.starts_with(n));
    let mut listed: Vec<(&Path, NotReadKind)> = Vec::new();
    for p in &report.not_scanned {
        listed.push((p, if in_cloud(p) { NotReadKind::Cloud } else { NotReadKind::Skip }));
    }
    for s in &report.skipped {
        let kind = match s.reason {
            SkipReason::Excluded if in_cloud(&s.path) => NotReadKind::Cloud,
            _ if skip(&s.path) => NotReadKind::Skip,
            SkipReason::Excluded => NotReadKind::Prompting,
            SkipReason::Dataless => NotReadKind::Evicted,
            SkipReason::OtherFilesystem | SkipReason::Automount => NotReadKind::OtherDisk,
            SkipReason::Snapshot => NotReadKind::Snapshot,
            SkipReason::Changed => NotReadKind::Changed,
            SkipReason::HoldsSubvolume => continue,
        };
        listed.push((&s.path, kind));
    }
    listed.extend(report.inaccessible.iter().map(|i| (i.path.as_path(), NotReadKind::Refused)));
    let mut seen: HashSet<&Path> = HashSet::new();
    listed
        .into_iter()
        .filter(|(path, _)| seen.insert(path))
        .map(|(path, kind)| NotRead { path: path.to_path_buf(), kind })
        .collect()
}
