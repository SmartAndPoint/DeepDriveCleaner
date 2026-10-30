//! `scan`: walk + rules + project activity + default selection → [`ScanReport`].

pub mod after_clean;
pub mod backups;
pub mod contents;
pub mod project;
pub mod recheck;
pub mod volumes;

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::class::{Removal, RiskClass};
use crate::env::Env;
use crate::model::{
    ClassTotal, ExclusionOrigin, Finding, Inaccessible, LargeFile, NotSelected, Project, RemovalKind, SCHEMA_VERSION,
    ScanReport, SkippedPath, Totals,
};
use crate::rules::{Catalog, Classified, KEPT, RuleVisitor, TOMBSTONE, classify};
use crate::scan::{self, DirReader, Progress, ReadErrorKind, Visitor, WalkError, WalkOptions, WalkSetupError};
use crate::stats;
use contents::{ContentsLimits, EntryIndex};
use volumes::{StatFs, SystemStatFs};

pub const DEFAULT_MIN_AGE: Duration = Duration::from_secs(14 * 86_400);

#[derive(Debug, Clone)]
pub struct ScanOptions {
    /// Empty = the home directory.
    pub roots: Vec<PathBuf>,
    /// Paths protected as `user.kept` (ADR §2).
    pub excludes: Vec<PathBuf>,
    /// `rebuildable` findings idle at least this long are selected by default.
    pub min_age: Duration,
    /// Rules switched off in `settings.json`: their findings are `ignored`.
    pub disabled_rules: Vec<String>,
    /// Findings smaller than this are `ignored` (`settings.json`'s `min_size`).
    pub min_size: u64,
    /// Files at least this large outside findings are listed.
    pub large_file_bytes: u64,
    /// Directories that are not even opened (background scans skip TCC areas).
    pub skip: Vec<PathBuf>,
    /// The user's skips, canonical (`Settings::resolve`): not opened, and
    /// listed in the report's `not_scanned`. They are in `excludes` too.
    pub user_skips: Vec<PathBuf>,
    /// The `user_skips` only `--skip` gave; the others are the settings'.
    pub flag_skips: Vec<PathBuf>,
    /// Where the `excludes` that are not skips came from: `settings.json`,
    /// or `--exclude` in its place.
    pub excludes_origin: ExclusionOrigin,
    /// How much of each user-data folder `contents` lists.
    pub contents: ContentsLimits,
    pub walk: WalkOptions,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            excludes: Vec::new(),
            min_age: DEFAULT_MIN_AGE,
            disabled_rules: Vec::new(),
            min_size: 0,
            large_file_bytes: crate::rules::LARGE_FILE_BYTES,
            skip: Vec::new(),
            user_skips: Vec::new(),
            flag_skips: Vec::new(),
            excludes_origin: ExclusionOrigin::Settings,
            contents: ContentsLimits::default(),
            walk: WalkOptions::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error(transparent)]
    Walk(#[from] WalkSetupError),
    /// The platform is not supported: refused before any walk, so never
    /// `Walk(WalkSetupError::Unsupported)` (ADR "Platform seam: one platform
    /// module per OS with declared capabilities", §5).
    #[error("{0}")]
    Unsupported(crate::platform::Unsupported),
    #[error("cannot resolve --exclude {path}: {source}")]
    Exclude {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub fn scan(env: &Env, opts: &ScanOptions, progress: &Progress) -> Result<ScanReport, ScanError> {
    scan_with(env, opts, progress, &SystemStatFs)
}

/// [`scan`](fn@scan) with the volume statistics read through `statfs`.
pub fn scan_with(
    env: &Env,
    opts: &ScanOptions,
    progress: &Progress,
    statfs: &dyn StatFs,
) -> Result<ScanReport, ScanError> {
    scan_with_entries(env, opts, progress, statfs).map(|(report, _)| report)
}

/// [`scan_with`], and every entry of the user's own folders with its state:
/// `clean --items` checks a picked entry against all of them, not against
/// the report's top 100 (story "User data: pick your own files for the Trash").
pub fn scan_with_entries(
    env: &Env,
    opts: &ScanOptions,
    progress: &Progress,
    statfs: &dyn StatFs,
) -> Result<(ScanReport, EntryIndex), ScanError> {
    scan_inner(env, opts, progress, statfs, None, None).map(|s| (s.report, s.index))
}

/// [`scan_with`] listing every directory through `reader` instead of the one
/// `opts.walk` names: a test lists each directory in another order and
/// expects the same report (story "Coverage floor and property tests for the
/// safety invariants").
pub fn scan_with_reader(
    env: &Env,
    opts: &ScanOptions,
    progress: &Progress,
    statfs: &dyn StatFs,
    reader: &dyn DirReader,
) -> Result<ScanReport, ScanError> {
    scan_inner(env, opts, progress, statfs, Some(reader), None).map(|s| s.report)
}

/// A scan with what the clean's re-check needs besides the report.
pub(crate) struct Scanned {
    pub(crate) report: ScanReport,
    pub(crate) index: EntryIndex,
    /// Every recorded read failure, `inaccessible`'s and the others.
    pub(crate) errors: Vec<WalkError>,
}

/// The one scan pipeline. `reader` replaces the one `opts.walk` names (a
/// test counts the directories opened); `reach` prunes the walk to a clean's
/// list (story "Faster clean: re-check only what the selection needs"), with
/// the same rules, classification and entries.
pub(crate) fn scan_inner(
    env: &Env,
    opts: &ScanOptions,
    progress: &Progress,
    statfs: &dyn StatFs,
    reader: Option<&dyn DirReader>,
    reach: Option<&recheck::Reach>,
) -> Result<Scanned, ScanError> {
    crate::platform::require_supported().map_err(ScanError::Unsupported)?;
    let started = Instant::now();
    let roots = roots_of(env, opts);
    let excludes = opts
        .excludes
        .iter()
        .map(|p| p.canonicalize().map_err(|source| ScanError::Exclude { path: p.clone(), source }))
        .collect::<Result<Vec<_>, _>>()?;
    let catalog = Catalog::builtin(env.home());
    let visitor = RuleVisitor::new(&catalog, excludes)
        .with_large_file_threshold(opts.large_file_bytes)
        .with_skips(opts.skip.clone())
        .with_user_skips(opts.user_skips.clone())
        .with_origins(opts.excludes_origin, opts.flag_skips.clone());
    let pruned = reach.map(|reach| recheck::Pruned { inner: &visitor, reach });
    let walking: &dyn Visitor = match &pruned {
        Some(pruned) => pruned,
        None => &visitor,
    };
    let out = match reader {
        Some(reader) => scan::walk_with_reader(&roots, walking, &opts.walk, progress, reader)?,
        None => scan::walk(&roots, walking, &opts.walk, progress)?,
    };

    // One read per folder: many findings can share a project root that holds
    // hundreds of thousands of entries (a temp folder).
    let mut activity = HashMap::new();
    let mut findings: Vec<Finding> =
        classify(&out, &visitor).iter().map(|c| finding(c, env.now(), opts, &mut activity)).collect();
    sort_findings(&mut findings);
    let markers = visitor.keep_markers();
    let cx = contents::Context {
        home: env.home(),
        now: env.now(),
        excludes: visitor.excludes(),
        markers: &markers,
        limits: opts.contents,
    };
    let index = contents::attach(&mut findings, &out.entries, &cx);

    let totals = totals(&findings, &out);
    let large_files = visitor.large_files().into_iter().map(|(bytes, path)| LargeFile { path, bytes }).collect();
    let inaccessible = out
        .errors
        .iter()
        .filter(|e| e.error.kind == ReadErrorKind::PermissionDenied)
        .map(|e| Inaccessible { path: e.path.clone(), error: e.error.to_string() })
        .collect();
    let skipped = out.skipped.iter().map(|s| SkippedPath { path: s.path.clone(), reason: s.reason }).collect();
    let roots: Vec<&Path> = out.roots.iter().map(|r| r.path.as_path()).collect();

    let report = ScanReport {
        schema_version: SCHEMA_VERSION,
        ddc_version: env!("CARGO_PKG_VERSION").to_owned(),
        platform: crate::platform::PLATFORM,
        capabilities: crate::platform::CAPABILITIES.to_vec(),
        generated_at: humantime::format_rfc3339_seconds(env.now()).to_string(),
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        home: env.home().to_path_buf(),
        scan_roots: out.roots.iter().map(|r| r.path.clone()).collect(),
        full_disk_access: full_disk_access(env),
        volumes: volumes::volumes(&roots, statfs),
        min_age_days: opts.min_age.as_secs() / 86_400,
        totals,
        findings,
        large_files,
        inaccessible,
        errors_total: out.errors_total,
        skipped,
        skipped_total: out.skipped_total,
        cancelled: out.cancelled,
        not_scanned: visitor.not_scanned(),
    };
    Ok(Scanned { report, index, errors: out.errors })
}

/// The roots a scan walks: the home when none are given.
pub(crate) fn roots_of(env: &Env, opts: &ScanOptions) -> Vec<PathBuf> {
    if opts.roots.is_empty() { vec![env.home().to_path_buf()] } else { opts.roots.clone() }
}

fn finding(
    c: &Classified,
    now: SystemTime,
    opts: &ScanOptions,
    activity: &mut HashMap<PathBuf, Option<SystemTime>>,
) -> Finding {
    let min_age = opts.min_age;
    // Activity of the project root and of the artifact's own directory: a
    // fresh manifest next to the artifact counts even under a stale git root.
    let mut last_activity =
        |dir: &Path| *activity.entry(dir.to_path_buf()).or_insert_with(|| project::last_activity(dir));
    let last = c.project_root.as_deref().and_then(|root| {
        let near = c.owner.path.parent().filter(|p| *p != root).and_then(&mut last_activity);
        last_activity(root).max(near)
    });
    let project = c.project_root.as_ref().map(|root| Project {
        root: root.clone(),
        last_activity: last.and_then(rfc3339),
        idle_days: last.map(|t| now.duration_since(t).unwrap_or_default().as_secs() / 86_400),
        has_lockfile: c.has_lockfile,
    });
    let not_selected = match c.class {
        RiskClass::Safe => None,
        RiskClass::Rebuildable => match last.map(|t| now.duration_since(t).unwrap_or_default()) {
            None => Some(NotSelected::UnknownActivity),
            Some(idle) if idle < min_age => Some(NotSelected::ActiveProject { idle_days: idle.as_secs() / 86_400 }),
            Some(_) => None,
        },
        RiskClass::Review => Some(NotSelected::Review),
        RiskClass::UserData => Some(NotSelected::UserData),
    };
    let ignored = hidden(c.rule.id, c.owner.bytes, opts);
    Finding {
        path: c.owner.path.clone(),
        rule: c.rule.id.to_owned(),
        category: c.rule.category,
        class: c.class,
        rule_class: c.rule.class,
        // The effective method: a `delete` rule downgraded to review goes to the Trash (ADR §1).
        removal: match c.rule.removal {
            Removal::Delete if c.class == RiskClass::Review => RemovalKind::Trash,
            ref removal => removal.into(),
        },
        bytes: c.owner.bytes,
        shared_bytes: c.owner.shared_bytes,
        private_bytes: c.owner.private_bytes,
        files: c.owner.files,
        dirs: c.owner.dirs,
        incomplete: c.owner.incomplete(),
        project,
        selected: not_selected.is_none() && !ignored,
        not_selected,
        ignored,
        notes: c.notes.clone(),
        hint: c.rule.hint.to_owned(),
        dev: c.owner.dev,
        ino: c.owner.ino,
        parent_dev: Some(c.owner.parent_dev),
        parent_ino: Some(c.owner.parent_ino),
        contents: None,
        protection: c.protection.clone(),
        // A protected finding has no date, also one that became `user.kept`
        // only after its walk (a marker or an exclusion inside it), whose
        // claim still read the times.
        modified: c.owner.modified.filter(|_| c.rule.id != KEPT).and_then(rfc3339_secs),
    }
}

/// The report's order: by class, then size (largest first), then path.
pub(crate) fn sort_findings(findings: &mut [Finding]) {
    findings
        .sort_by(|a, b| a.class.cmp(&b.class).then(Reverse(a.bytes).cmp(&Reverse(b.bytes))).then(a.path.cmp(&b.path)));
}

/// Whether `settings.json` hides a finding of `rule` that holds `bytes`.
/// ddc's own leftovers of an interrupted clean are never hidden: they would
/// stay on disk for good.
pub(crate) fn hidden(rule: &str, bytes: u64, opts: &ScanOptions) -> bool {
    rule != TOMBSTONE && (opts.disabled_rules.iter().any(|r| r == rule) || bytes < opts.min_size)
}

fn totals(findings: &[Finding], out: &scan::WalkOutput) -> Totals {
    // Claims that are not findings (app bundles, global packages, protected
    // toolchain versions) still occupy space: report it as outside findings.
    // A set: a tree like /System holds tens of thousands of claims and findings.
    let found: HashSet<&Path> = findings.iter().map(|f| f.path.as_path()).collect();
    let hidden_bytes: u64 = out
        .owners
        .iter()
        .filter(|o| {
            o.kind == scan::OwnerKind::Claim && {
                stats::add(stats::Counter::ClaimLookups, 1);
                !found.contains(o.path.as_path())
            }
        })
        .map(|o| o.bytes)
        .sum();
    let mut totals = Totals {
        scanned_bytes: out.total_bytes,
        files: out.roots.iter().map(|r| r.files).sum(),
        dirs: out.roots.iter().map(|r| r.dirs).sum(),
        unowned_bytes: out.unowned_bytes + hidden_bytes,
        classes: Vec::new(),
        selected_bytes: 0,
        selected_reclaimable_bytes: 0,
        ignored_count: 0,
        ignored_bytes: 0,
    };
    count_findings(&mut totals, findings);
    totals
}

/// The totals that follow from the findings alone: per class, selected and ignored.
pub(crate) fn count_findings(totals: &mut Totals, findings: &[Finding]) {
    let (ignored, shown): (Vec<&Finding>, Vec<&Finding>) = findings.iter().partition(|f| f.ignored);
    totals.classes = RiskClass::ALL
        .iter()
        .map(|&class| {
            let of: Vec<&Finding> = shown.iter().copied().filter(|f| f.class == class).collect();
            let selected: Vec<&&Finding> = of.iter().filter(|f| f.selected).collect();
            ClassTotal {
                class,
                count: of.len() as u64,
                bytes: of.iter().map(|f| f.bytes).sum(),
                selected_count: selected.len() as u64,
                selected_bytes: selected.iter().map(|f| f.bytes).sum(),
            }
        })
        .collect();
    let selected = shown.iter().copied().filter(|f| f.selected);
    totals.selected_bytes = selected.clone().map(|f| f.bytes).sum();
    totals.selected_reclaimable_bytes = selected.map(Finding::reclaimable_bytes).sum();
    totals.ignored_count = ignored.len() as u64;
    totals.ignored_bytes = ignored.iter().map(|f| f.bytes).sum();
}

/// TCC.db is readable only with Full Disk Access.
/// Whether this process has Full Disk Access (it can read the TCC database).
pub fn full_disk_access(env: &Env) -> bool {
    fs::File::open(env.home().join("Library/Application Support/com.apple.TCC/TCC.db")).is_ok()
}

/// A file time as RFC 3339, `None` before 1970 or past year 9999: humantime
/// cannot write those, and a file can carry any time.
pub(crate) fn rfc3339(t: SystemTime) -> Option<String> {
    let secs = t.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    (secs < backups::YEAR_10000).then(|| humantime::format_rfc3339_seconds(t).to_string())
}

/// A file time in Unix seconds as RFC 3339, as [`rfc3339`]: fixed-width UTC
/// to the second (`2024-01-01T00:00:00Z`), so the strings compare as the times
/// do. `None` before 1970 or past year 9999.
pub(crate) fn rfc3339_secs(secs: i64) -> Option<String> {
    rfc3339(std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(u64::try_from(secs).ok()?))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_secs_is_fixed_width_utc_within_the_years_it_can_write() {
        let last = backups::YEAR_10000 as i64 - 1;
        assert_eq!(rfc3339_secs(0).as_deref(), Some("1970-01-01T00:00:00Z"));
        assert_eq!(rfc3339_secs(1_704_067_210).as_deref(), Some("2024-01-01T00:00:10Z"));
        assert_eq!(rfc3339_secs(last).as_deref(), Some("9999-12-31T23:59:59Z"));
        for secs in [0, 1, 1_704_067_210, last] {
            assert_eq!(rfc3339_secs(secs).map(|s| s.len()), Some(20), "{secs}");
        }
        assert_eq!(rfc3339_secs(-1), None, "before 1970");
        assert_eq!(rfc3339_secs(backups::YEAR_10000 as i64), None, "past year 9999");
        assert_eq!(rfc3339_secs(i64::MAX), None);
    }
}
