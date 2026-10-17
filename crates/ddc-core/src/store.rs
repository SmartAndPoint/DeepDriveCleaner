//! Data directory: the scan/clean lock and the last scan report.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use crate::env::Env;
use crate::model::ScanReport;

pub const LAST_SCAN: &str = "last-scan.json";
const LOCK: &str = "lock";

/// A held lock on `<data-dir>/lock`; released on drop.
#[derive(Debug)]
pub struct DataLock {
    _file: File,
}

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("another ddc scan or clean is running (lock {0})")]
    Busy(PathBuf),
    #[error("cannot lock {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// Scans share the lock; a cleanup takes it exclusively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    Shared,
    Exclusive,
}

/// Takes the data-dir lock without waiting.
pub fn try_lock(env: &Env, mode: LockMode) -> Result<DataLock, LockError> {
    let path = env.data_dir().join(LOCK);
    let io_err = |source| LockError::Io { path: path.clone(), source };
    fs::create_dir_all(env.data_dir()).map_err(io_err)?;
    let file = OpenOptions::new().create(true).truncate(false).write(true).open(&path).map_err(io_err)?;
    let res = match mode {
        LockMode::Shared => file.try_lock_shared(),
        LockMode::Exclusive => file.try_lock(),
    };
    match res {
        Ok(()) => Ok(DataLock { _file: file }),
        Err(fs::TryLockError::WouldBlock) => Err(LockError::Busy(path)),
        Err(fs::TryLockError::Error(e)) => Err(io_err(e)),
    }
}

/// Writes `<data-dir>/last-scan.json` atomically (temp file in the same
/// directory, fsync, rename). Returns the final path.
pub fn write_last_scan(env: &Env, report: &ScanReport) -> io::Result<PathBuf> {
    let json = serde_json::to_vec_pretty(report).map_err(io::Error::other)?;
    write_atomic(&env.data_dir().join(LAST_SCAN), &json)
}

/// The saved report, or `None` when absent or written by another schema version.
pub fn read_last_scan(env: &Env) -> io::Result<Option<ScanReport>> {
    match fs::read(env.data_dir().join(LAST_SCAN)) {
        Ok(bytes) => {
            let version = serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|v| v.get("schema_version")?.as_u64());
            if version != Some(u64::from(crate::model::SCHEMA_VERSION)) {
                return Ok(None);
            }
            serde_json::from_slice(&bytes).map(Some).map_err(io::Error::other)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Atomic replace of `path` with `bytes`; the temp file is removed on failure.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<PathBuf> {
    let dir = path.parent().ok_or_else(|| io::Error::other("path has no parent"))?;
    fs::create_dir_all(dir)?;
    let name = path.file_name().ok_or_else(|| io::Error::other("path has no file name"))?;
    let tmp = dir.join(format!(".{}.tmp-{}", name.to_string_lossy(), std::process::id()));
    let result = (|| {
        let mut file = File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        #[allow(clippy::disallowed_methods)] // our own temp file
        let _ = fs::remove_file(&tmp);
    }
    result.map(|()| path.to_path_buf())
}
