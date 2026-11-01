//! The home row from `last-scan.json`, when that report still describes the
//! whole home folder.

use crate::platform::Canonical as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::env::Env;
use crate::model::SCHEMA_VERSION;
use crate::report::volumes::StatFs;
use crate::store::LAST_SCAN;

/// What the home row takes from the last scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct LastScan {
    pub generated_at: String,
    pub bytes: u64,
    pub reclaimable_bytes: u64,
    pub unreadable: u64,
}

/// The last scan, when it has the current schema, was not stopped, scanned
/// exactly the home folder, put it on the volume mounted where `data_root`
/// is, and is at most `max_age` old.
pub(super) fn last_scan(env: &Env, statfs: &dyn StatFs, data_root: &Path, max_age: Duration) -> Option<LastScan> {
    #[derive(serde::Deserialize)]
    struct Head {
        schema_version: u32,
        generated_at: String,
        scan_roots: Vec<PathBuf>,
        #[serde(default)]
        volumes: Vec<HeadVolume>,
        totals: HeadTotals,
        errors_total: u64,
        cancelled: bool,
    }
    #[derive(serde::Deserialize)]
    struct HeadVolume {
        mount_point: PathBuf,
        roots: Vec<PathBuf>,
    }
    #[derive(serde::Deserialize)]
    struct HeadTotals {
        scanned_bytes: u64,
        selected_reclaimable_bytes: u64,
    }
    let bytes = std::fs::read(env.data_dir().join(LAST_SCAN)).ok()?;
    let head: Head = serde_json::from_slice(&bytes).ok()?;
    if head.schema_version != SCHEMA_VERSION || head.cancelled {
        return None;
    }
    let home = canonical(env.home());
    let [root] = head.scan_roots.as_slice() else { return None };
    if canonical(root) != home {
        return None;
    }
    let data_mount = statfs.statfs(data_root).ok()?.mount_point;
    if !head.volumes.iter().any(|v| v.mount_point == data_mount && v.roots.contains(root)) {
        return None;
    }
    let at = humantime::parse_rfc3339(&head.generated_at).ok()?;
    // A report from the future is not trusted either.
    if env.now().duration_since(at).ok()? > max_age {
        return None;
    }
    Some(LastScan {
        generated_at: head.generated_at,
        bytes: head.totals.scanned_bytes,
        reclaimable_bytes: head.totals.selected_reclaimable_bytes,
        unreadable: head.errors_total,
    })
}

fn canonical(path: &Path) -> PathBuf {
    path.canonical().unwrap_or_else(|_| path.to_path_buf())
}
