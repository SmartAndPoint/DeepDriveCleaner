//! Data directory: the scan/clean lock, the last scan report and the one
//! before it.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::disk::DiskReport;
use crate::env::Env;
use crate::model::{ScanReport, WrittenBy};

pub const LAST_SCAN: &str = "last-scan.json";
/// The report a `scan`, `background` or `clean-recheck` save replaced, when
/// it was whole ([`write_last_scan`]).
pub const PREVIOUS_SCAN: &str = "previous-scan.json";
pub const LAST_DISK: &str = "last-disk.json";
/// `ddc space`'s samples of the free space (`space::record`); no lock.
pub const SPACE: &str = "space.json";
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

/// A save of `last-scan.json`: where it went and what became of the report
/// it replaced.
#[derive(Debug)]
pub struct Saved {
    pub path: PathBuf,
    pub kept: Kept,
}

/// What a save did with the report it replaced.
#[derive(Debug)]
pub enum Kept {
    /// Moved to `previous-scan.json`, with its `written_at`: `None` for a
    /// report an older ddc saved.
    Kept { written_at: Option<String> },
    /// Nothing moved: the save does not keep, no report was there, or it
    /// was not whole. `previous-scan.json` is as it was.
    Nothing,
    /// The report was whole, but moving it failed; the save went on, and
    /// `previous-scan.json` is as it was.
    Failed(io::Error),
}

/// Saves `report` to `<data-dir>/last-scan.json` (ADR "Menu bar app: Swift
/// shell over the ddc CLI", §6 item 14), in three steps:
///
/// 1. **Keep.** A `scan`, `background` or `clean-recheck` save moves the
///    report it replaces to `previous-scan.json` when that report is whole:
///    its head reads, it is not `cancelled`, and its home and roots are the
///    env's home and `default_roots`, the roots of a scan without paths
///    (`None` when they do not resolve: nothing is kept). The incoming
///    report's roots do not matter. `last-scan.json` is hard-linked (copied
///    where links are refused) to a temp name, the head is read from that
///    copy, and the copy is renamed onto `previous-scan.json`, so
///    `last-scan.json` is never missing. Every other save leaves
///    `previous-scan.json` as it is.
/// 2. **Stamp** `report`: `written_by`, `written_at` (now, to the second),
///    and `previous_written_at`: the kept report's `written_at`, else the
///    replaced report's own `previous_written_at`, carried forward.
/// 3. **Write** it atomically ([`write_atomic`]).
///
/// A failure to keep never fails the save: it is in [`Saved::kept`], for the
/// caller to warn about.
pub fn write_last_scan(
    env: &Env,
    report: &mut ScanReport,
    writer: WrittenBy,
    default_roots: Option<&[PathBuf]>,
) -> io::Result<Saved> {
    let copies = Copies { link: &|from, to| fs::hard_link(from, to), copy: &|from, to| fs::copy(from, to).map(drop) };
    write_last_scan_with(env, report, writer, default_roots, &copies)
}

/// How [`keep`] puts `last-scan.json` at its temp name: a hard link, else a
/// copy. A test refuses either.
struct Copies<'a> {
    link: &'a dyn Fn(&Path, &Path) -> io::Result<()>,
    copy: &'a dyn Fn(&Path, &Path) -> io::Result<()>,
}

/// [`write_last_scan`] with the copy steps injected.
fn write_last_scan_with(
    env: &Env,
    report: &mut ScanReport,
    writer: WrittenBy,
    default_roots: Option<&[PathBuf]>,
    copies: &Copies<'_>,
) -> io::Result<Saved> {
    let last = env.data_dir().join(LAST_SCAN);
    let (kept, replaced) =
        if writer.keeps() { keep(env, &last, default_roots, copies) } else { (Kept::Nothing, Head::read(&last)) };
    report.written_by = Some(writer);
    report.written_at = Some(humantime::format_rfc3339_seconds(SystemTime::now()).to_string());
    report.previous_written_at = match &kept {
        Kept::Kept { written_at } => written_at.clone(),
        Kept::Nothing | Kept::Failed(_) => replaced.and_then(|h| h.previous_written_at),
    };
    let json = serde_json::to_vec_pretty(report).map_err(io::Error::other)?;
    let path = write_atomic(&last, &json)?;
    Ok(Saved { path, kept })
}

/// Step 1 of [`write_last_scan`]: what was kept, and the head of the
/// report `last` held (`None` when absent or unreadable).
fn keep(env: &Env, last: &Path, default_roots: Option<&[PathBuf]>, copies: &Copies<'_>) -> (Kept, Option<Head>) {
    let tmp = env.data_dir().join(format!(".{PREVIOUS_SCAN}.tmp-{}", std::process::id()));
    // A run killed here before, under the same pid, would make the link fail.
    remove_temp(&tmp);
    let copied = match (copies.link)(last, &tmp) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return (Kept::Nothing, None),
        Err(_) => (copies.copy)(last, &tmp),
        Ok(()) => Ok(()),
    };
    if let Err(e) = copied {
        remove_temp(&tmp);
        return match e.kind() {
            io::ErrorKind::NotFound => (Kept::Nothing, None),
            _ => (Kept::Failed(e), Head::read(last)),
        };
    }
    let head = Head::read(&tmp);
    let whole = head.as_ref().is_some_and(|h| {
        !h.cancelled && h.home == env.home() && default_roots.is_some_and(|roots| roots == h.scan_roots.as_slice())
    });
    if !whole {
        remove_temp(&tmp);
        return (Kept::Nothing, head);
    }
    let written_at = head.as_ref().and_then(|h| h.written_at.clone());
    let kept = match fs::rename(&tmp, env.data_dir().join(PREVIOUS_SCAN)) {
        Ok(()) => Kept::Kept { written_at },
        Err(e) => Kept::Failed(e),
    };
    // After a failure, and after a rename between two links of one file
    // (a save that kept and then failed to write), which leaves both names.
    if fs::symlink_metadata(&tmp).is_ok() {
        remove_temp(&tmp);
    }
    (kept, head)
}

/// What [`keep`] reads of a report, of any schema version.
#[derive(serde::Deserialize)]
struct Head {
    home: PathBuf,
    scan_roots: Vec<PathBuf>,
    #[serde(default)]
    cancelled: bool,
    written_at: Option<String>,
    previous_written_at: Option<String>,
}

impl Head {
    fn read(path: &Path) -> Option<Self> {
        serde_json::from_slice(&fs::read(path).ok()?).ok()
    }
}

/// Removes a temp file of our own, if it is there.
fn remove_temp(path: &Path) {
    #[allow(clippy::disallowed_methods)] // our own temp file
    let _ = fs::remove_file(path);
}

/// Writes `<data-dir>/last-disk.json` atomically. No lock: `ddc disk` only
/// reads what a clean writes, and when two runs overlap the last to finish
/// wins (story "Whole disk: what lies outside the home folder").
pub fn write_last_disk(env: &Env, report: &DiskReport) -> io::Result<PathBuf> {
    let json = serde_json::to_vec_pretty(report).map_err(io::Error::other)?;
    write_atomic(&env.data_dir().join(LAST_DISK), &json)
}

/// The saved disk report, or `None` when absent or written by another
/// schema version (`ddc disk free` edits it).
pub fn read_last_disk(env: &Env) -> io::Result<Option<DiskReport>> {
    match fs::read(env.data_dir().join(LAST_DISK)) {
        Ok(bytes) => {
            let version = serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|v| v.get("schema_version")?.as_u64());
            if version != Some(u64::from(crate::disk::DISK_SCHEMA_VERSION)) {
                return Ok(None);
            }
            serde_json::from_slice(&bytes).map(Some).map_err(io::Error::other)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The saved report, or `None` when absent or written by another schema version.
pub fn read_last_scan(env: &Env) -> io::Result<Option<ScanReport>> {
    read_scan(env, LAST_SCAN)
}

/// The kept report before the saved one ([`PREVIOUS_SCAN`]), or `None` when
/// absent or written by another schema version.
pub fn read_previous_scan(env: &Env) -> io::Result<Option<ScanReport>> {
    read_scan(env, PREVIOUS_SCAN)
}

fn read_scan(env: &Env, name: &str) -> io::Result<Option<ScanReport>> {
    match fs::read(env.data_dir().join(name)) {
        Ok(bytes) => decode_last_scan(&bytes),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// The report in the bytes of `last-scan.json`, or `None` when they are not
/// JSON or carry another schema version.
pub fn decode_last_scan(bytes: &[u8]) -> io::Result<Option<ScanReport>> {
    let version =
        serde_json::from_slice::<serde_json::Value>(bytes).ok().and_then(|v| v.get("schema_version")?.as_u64());
    if version != Some(u64::from(crate::model::SCHEMA_VERSION)) {
        return Ok(None);
    }
    serde_json::from_slice(bytes).map(Some).map_err(io::Error::other)
}

/// The saved report's file count, read without the rest of it: the
/// expected count of a clean's re-check, before the re-check replaces the
/// file (story "CLI: progress events"). `None` when absent, unreadable or of
/// another schema version.
pub fn last_scan_files(env: &Env) -> Option<u64> {
    #[derive(serde::Deserialize)]
    struct Head {
        schema_version: u32,
        totals: HeadTotals,
    }
    #[derive(serde::Deserialize)]
    struct HeadTotals {
        files: u64,
    }
    let bytes = fs::read(env.data_dir().join(LAST_SCAN)).ok()?;
    let head = serde_json::from_slice::<Head>(&bytes).ok()?;
    (head.schema_version == crate::model::SCHEMA_VERSION).then_some(head.totals.files)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A home with a project folder, and a data folder of its own.
    struct Fx {
        _dir: tempfile::TempDir,
        env: Env,
    }

    impl Fx {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            fs::create_dir_all(dir.path().join("home/p")).unwrap();
            let data = dir.path().join("data");
            let env = Env::with_paths(&dir.path().join("home"), &data, &data.join("logs"), SystemTime::now()).unwrap();
            Self { _dir: dir, env }
        }

        fn home(&self) -> PathBuf {
            self.env.home().to_path_buf()
        }

        /// The roots of a scan without paths.
        fn defaults(&self) -> Vec<PathBuf> {
            vec![self.home()]
        }

        fn path(&self, name: &str) -> PathBuf {
            self.env.data_dir().join(name)
        }

        /// A report of `roots`, saved at `written_at`.
        fn report(&self, roots: &[PathBuf], written_at: Option<&str>, previous: Option<&str>) -> ScanReport {
            let mut r: ScanReport = serde_json::from_str(
                r#"{"schema_version":8,"ddc_version":"0","platform":"linux","capabilities":[],
                   "generated_at":"2026-11-01T10:00:00Z","duration_ms":0,"home":"/h","scan_roots":[],
                   "full_disk_access":true,"min_age_days":14,
                   "totals":{"scanned_bytes":0,"files":0,"dirs":0,"unowned_bytes":0,"classes":[],
                     "selected_bytes":0,"selected_reclaimable_bytes":0},
                   "findings":[],"large_files":[],"inaccessible":[],"errors_total":0,"skipped":[],
                   "skipped_total":0,"cancelled":false}"#,
            )
            .unwrap();
            r.home = self.home();
            r.scan_roots = roots.to_vec();
            r.written_by = written_at.map(|_| WrittenBy::Scan);
            r.written_at = written_at.map(str::to_owned);
            r.previous_written_at = previous.map(str::to_owned);
            r
        }

        /// Puts `bytes` at `name` in the data folder; returns them.
        fn put(&self, name: &str, bytes: &[u8]) -> Vec<u8> {
            fs::create_dir_all(self.env.data_dir()).unwrap();
            fs::write(self.path(name), bytes).unwrap();
            bytes.to_vec()
        }

        /// Puts `r` at `name` as a save writes it; returns its bytes.
        fn put_report(&self, name: &str, r: &ScanReport) -> Vec<u8> {
            self.put(name, &serde_json::to_vec_pretty(r).unwrap())
        }

        fn save(&self, writer: WrittenBy) -> (ScanReport, Kept) {
            let mut r = self.report(&self.defaults(), None, None);
            let defaults = self.defaults();
            let saved = write_last_scan(&self.env, &mut r, writer, Some(&defaults)).unwrap();
            assert_eq!(saved.path, self.path(LAST_SCAN));
            (r, saved.kept)
        }

        fn previous(&self) -> Option<Vec<u8>> {
            fs::read(self.path(PREVIOUS_SCAN)).ok()
        }

        /// The data folder's file names, but for the logs folder.
        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = crate::stats::read_dir(self.env.data_dir())
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| n != "logs")
                .collect();
            names.sort();
            names
        }

        /// The saved report, as `ddc scan --json` would print it.
        fn last(&self) -> ScanReport {
            serde_json::from_slice(&fs::read(self.path(LAST_SCAN)).unwrap()).unwrap()
        }
    }

    const A: &str = "2026-11-01T10:00:05Z";
    const B: &str = "2026-11-02T10:00:05Z";

    fn assert_kept(kept: &Kept, want: Option<&str>) {
        match kept {
            Kept::Kept { written_at } => assert_eq!(written_at.as_deref(), want),
            other => panic!("kept nothing: {other:?}"),
        }
    }

    /// RFC 3339 in UTC, to the second, and not before `since`.
    fn assert_stamped(r: &ScanReport, writer: WrittenBy, since: SystemTime) {
        assert_eq!(r.written_by, Some(writer));
        let at = r.written_at.as_deref().unwrap();
        assert_eq!(at.len(), "2026-11-01T10:00:05Z".len(), "{at}");
        assert!(at.ends_with('Z'), "{at}");
        let t = humantime::parse_rfc3339(at).unwrap();
        assert!(t + std::time::Duration::from_secs(1) > since, "{at}");
    }

    #[test]
    fn a_scan_background_or_clean_recheck_save_keeps_a_whole_report_byte_for_byte() {
        for writer in [WrittenBy::Scan, WrittenBy::Background, WrittenBy::CleanRecheck] {
            let fx = Fx::new();
            let outgoing = fx.put_report(LAST_SCAN, &fx.report(&fx.defaults(), Some(A), Some(B)));
            let since = SystemTime::now();
            let (r, kept) = fx.save(writer);
            assert_kept(&kept, Some(A));
            assert_eq!(fx.previous(), Some(outgoing), "{writer:?}");
            assert_eq!(r.previous_written_at.as_deref(), Some(A));
            assert_stamped(&r, writer, since);
            assert_eq!(fx.last(), r, "the saved file is the stamped report");
            assert_eq!(fx.names(), [LAST_SCAN, PREVIOUS_SCAN], "no temp file is left");
        }
    }

    #[test]
    fn the_incoming_reports_roots_do_not_change_what_is_kept() {
        let fx = Fx::new();
        let outgoing = fx.put_report(LAST_SCAN, &fx.report(&fx.defaults(), Some(A), None));
        let mut project = fx.report(&[fx.home().join("p")], None, None);
        let defaults = fx.defaults();
        let saved = write_last_scan(&fx.env, &mut project, WrittenBy::Scan, Some(&defaults)).unwrap();
        assert_kept(&saved.kept, Some(A));
        assert_eq!(fx.previous(), Some(outgoing));
        // The next project scan finds a project report: it keeps nothing.
        let kept_before = fx.previous();
        let mut again = fx.report(&[fx.home().join("p")], None, None);
        let saved = write_last_scan(&fx.env, &mut again, WrittenBy::Scan, Some(&defaults)).unwrap();
        assert!(matches!(saved.kept, Kept::Nothing), "{:?}", saved.kept);
        assert_eq!(fx.previous(), kept_before);
        assert_eq!(again.previous_written_at.as_deref(), Some(A), "carried forward from the project report");
    }

    #[test]
    fn a_clean_clean_items_or_disk_free_save_keeps_nothing_and_carries_the_pair_forward() {
        for writer in [WrittenBy::Clean, WrittenBy::CleanItems, WrittenBy::DiskFree] {
            let fx = Fx::new();
            let previous = fx.put_report(PREVIOUS_SCAN, &fx.report(&fx.defaults(), Some(A), None));
            fx.put_report(LAST_SCAN, &fx.report(&fx.defaults(), Some(B), Some(A)));
            let since = SystemTime::now();
            let (r, kept) = fx.save(writer);
            assert!(matches!(kept, Kept::Nothing), "{writer:?}: {kept:?}");
            assert_eq!(fx.previous(), Some(previous), "{writer:?}");
            assert_eq!(r.previous_written_at.as_deref(), Some(A), "{writer:?}");
            assert_stamped(&r, writer, since);
            assert_eq!(fx.names(), [LAST_SCAN, PREVIOUS_SCAN]);
        }
        // With no previous report there is none afterwards either.
        let fx = Fx::new();
        fx.put_report(LAST_SCAN, &fx.report(&fx.defaults(), Some(B), None));
        let (r, _) = fx.save(WrittenBy::CleanItems);
        assert_eq!(fx.previous(), None);
        assert_eq!(r.previous_written_at, None);
    }

    #[test]
    fn a_report_that_is_not_whole_or_not_the_default_ones_is_not_kept() {
        for why in ["cancelled", "other roots", "other home"] {
            let fx = Fx::new();
            let previous = fx.put(PREVIOUS_SCAN, b"the report before");
            let mut outgoing = fx.report(&fx.defaults(), Some(B), Some(A));
            match why {
                "cancelled" => outgoing.cancelled = true,
                "other roots" => outgoing.scan_roots = vec![fx.home().join("p")],
                _ => {
                    outgoing.home = fx.home().parent().unwrap().join("other");
                    outgoing.scan_roots = vec![outgoing.home.clone()];
                }
            }
            fx.put_report(LAST_SCAN, &outgoing);
            let (r, kept) = fx.save(WrittenBy::Scan);
            assert!(matches!(kept, Kept::Nothing), "{why}: {kept:?}");
            assert_eq!(fx.previous(), Some(previous), "{why}");
            assert_eq!(r.previous_written_at.as_deref(), Some(A), "{why}: carried forward");
            assert_eq!(fx.names(), [LAST_SCAN, PREVIOUS_SCAN], "{why}");
        }
        // A head that does not read: nothing to carry forward either.
        let fx = Fx::new();
        let previous = fx.put(PREVIOUS_SCAN, b"the report before");
        fx.put(LAST_SCAN, b"{\"home\": 1");
        let (r, kept) = fx.save(WrittenBy::Background);
        assert!(matches!(kept, Kept::Nothing), "{kept:?}");
        assert_eq!(fx.previous(), Some(previous));
        assert_eq!(r.previous_written_at, None);
        // Default roots that do not resolve keep nothing.
        let fx = Fx::new();
        fx.put_report(LAST_SCAN, &fx.report(&fx.defaults(), Some(B), Some(A)));
        let mut r = fx.report(&fx.defaults(), None, None);
        let saved = write_last_scan(&fx.env, &mut r, WrittenBy::Scan, None).unwrap();
        assert!(matches!(saved.kept, Kept::Nothing), "{:?}", saved.kept);
        assert_eq!(fx.previous(), None);
        assert_eq!(r.previous_written_at.as_deref(), Some(A));
    }

    #[test]
    fn a_report_of_any_schema_version_and_without_the_new_fields_is_kept() {
        let fx = Fx::new();
        let mut old = serde_json::to_value(fx.report(&fx.defaults(), None, None)).unwrap();
        old["schema_version"] = 3.into();
        let outgoing = fx.put(LAST_SCAN, &serde_json::to_vec_pretty(&old).unwrap());
        let (r, kept) = fx.save(WrittenBy::Scan);
        assert_kept(&kept, None);
        assert_eq!(fx.previous(), Some(outgoing));
        assert_eq!(r.previous_written_at, None);
        assert_eq!(read_previous_scan(&fx.env).unwrap(), None, "another schema version");
    }

    #[test]
    fn the_first_save_keeps_nothing_and_warns_of_nothing() {
        let fx = Fx::new();
        let (r, kept) = fx.save(WrittenBy::Scan);
        assert!(matches!(kept, Kept::Nothing), "{kept:?}");
        assert_eq!(r.previous_written_at, None);
        assert_eq!(fx.names(), [LAST_SCAN]);
        assert_eq!(read_previous_scan(&fx.env).unwrap(), None);
    }

    #[test]
    fn a_previous_report_that_cannot_be_replaced_fails_the_keep_not_the_save() {
        let fx = Fx::new();
        fs::create_dir_all(fx.path(PREVIOUS_SCAN)).unwrap();
        fx.put_report(LAST_SCAN, &fx.report(&fx.defaults(), Some(B), Some(A)));
        let since = SystemTime::now();
        let (r, kept) = fx.save(WrittenBy::Scan);
        assert!(matches!(kept, Kept::Failed(_)), "{kept:?}");
        assert_stamped(&r, WrittenBy::Scan, since);
        assert_eq!(r.previous_written_at.as_deref(), Some(A), "carried forward");
        assert_eq!(fx.last(), r);
        assert!(fx.path(PREVIOUS_SCAN).is_dir());
        assert_eq!(fx.names(), [LAST_SCAN, PREVIOUS_SCAN], "no temp file is left");
    }

    #[test]
    fn a_file_system_without_links_gets_a_copy() {
        let fx = Fx::new();
        let outgoing = fx.put_report(LAST_SCAN, &fx.report(&fx.defaults(), Some(A), None));
        let mut r = fx.report(&fx.defaults(), None, None);
        let defaults = fx.defaults();
        let refuse = |_: &Path, _: &Path| Err(io::Error::from(io::ErrorKind::Unsupported));
        let copy = |from: &Path, to: &Path| fs::copy(from, to).map(drop);
        let copies = Copies { link: &refuse, copy: &copy };
        let saved = write_last_scan_with(&fx.env, &mut r, WrittenBy::Scan, Some(&defaults), &copies).unwrap();
        assert_kept(&saved.kept, Some(A));
        assert_eq!(fx.previous(), Some(outgoing));
        assert_eq!(fx.names(), [LAST_SCAN, PREVIOUS_SCAN]);
        // And with no report to copy, nothing is kept and nothing is said.
        let fx = Fx::new();
        let mut r = fx.report(&fx.defaults(), None, None);
        let saved = write_last_scan_with(&fx.env, &mut r, WrittenBy::Scan, Some(&defaults), &copies).unwrap();
        assert!(matches!(saved.kept, Kept::Nothing), "{:?}", saved.kept);
    }

    #[test]
    fn a_report_that_can_be_neither_linked_nor_copied_fails_the_keep_not_the_save() {
        let fx = Fx::new();
        let previous = fx.put(PREVIOUS_SCAN, b"the report before");
        fx.put_report(LAST_SCAN, &fx.report(&fx.defaults(), Some(B), Some(A)));
        let mut r = fx.report(&fx.defaults(), None, None);
        let defaults = fx.defaults();
        let refuse = |_: &Path, _: &Path| Err(io::Error::from(io::ErrorKind::PermissionDenied));
        let copies = Copies { link: &refuse, copy: &refuse };
        let saved = write_last_scan_with(&fx.env, &mut r, WrittenBy::Scan, Some(&defaults), &copies).unwrap();
        assert!(matches!(saved.kept, Kept::Failed(_)), "{:?}", saved.kept);
        assert_eq!(r.previous_written_at.as_deref(), Some(A), "carried forward");
        assert_eq!(fx.previous(), Some(previous));
        assert_eq!(fx.last(), r, "the save replaced it all the same");
        assert_eq!(fx.names(), [LAST_SCAN, PREVIOUS_SCAN]);
    }

    #[test]
    fn a_temp_file_a_killed_run_left_under_the_same_pid_is_replaced() {
        let fx = Fx::new();
        let outgoing = fx.put_report(LAST_SCAN, &fx.report(&fx.defaults(), Some(A), None));
        fx.put(&format!(".{PREVIOUS_SCAN}.tmp-{}", std::process::id()), b"left behind");
        let (_, kept) = fx.save(WrittenBy::Scan);
        assert_kept(&kept, Some(A));
        assert_eq!(fx.previous(), Some(outgoing));
        assert_eq!(fx.names(), [LAST_SCAN, PREVIOUS_SCAN]);
    }

    #[test]
    fn a_pair_left_as_two_links_of_one_file_leaves_no_temp_file() {
        // A save that kept and then failed to write leaves both names on one file.
        let fx = Fx::new();
        let outgoing = fx.put_report(LAST_SCAN, &fx.report(&fx.defaults(), Some(A), None));
        fs::hard_link(fx.path(LAST_SCAN), fx.path(PREVIOUS_SCAN)).unwrap();
        let (r, kept) = fx.save(WrittenBy::Scan);
        assert_kept(&kept, Some(A));
        assert_eq!(fx.previous(), Some(outgoing));
        assert_eq!(r.previous_written_at.as_deref(), Some(A));
        assert_eq!(fx.names(), [LAST_SCAN, PREVIOUS_SCAN]);
    }
}
