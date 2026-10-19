//! `scan`: walk + rules + project activity + default selection → [`ScanReport`].

pub mod project;
pub mod volumes;

use std::cmp::Reverse;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::class::{Removal, RiskClass};
use crate::env::Env;
use crate::model::{
    ClassTotal, Finding, Inaccessible, LargeFile, NotSelected, Project, RemovalKind, SCHEMA_VERSION, ScanReport,
    SkippedPath, Totals,
};
use crate::rules::{Catalog, Classified, RuleVisitor, TOMBSTONE, classify};
use crate::scan::{self, Progress, ReadErrorKind, WalkOptions, WalkSetupError};
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
            walk: WalkOptions::default(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    #[error(transparent)]
    Walk(#[from] WalkSetupError),
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

/// [`scan`] with the volume statistics read through `statfs`.
pub fn scan_with(
    env: &Env,
    opts: &ScanOptions,
    progress: &Progress,
    statfs: &dyn StatFs,
) -> Result<ScanReport, ScanError> {
    let started = Instant::now();
    let roots = if opts.roots.is_empty() { vec![env.home().to_path_buf()] } else { opts.roots.clone() };
    let excludes = opts
        .excludes
        .iter()
        .map(|p| p.canonicalize().map_err(|source| ScanError::Exclude { path: p.clone(), source }))
        .collect::<Result<Vec<_>, _>>()?;
    let catalog = Catalog::builtin(env.home());
    let visitor = RuleVisitor::new(&catalog, excludes)
        .with_large_file_threshold(opts.large_file_bytes)
        .with_skips(opts.skip.clone());
    let out = scan::walk(&roots, &visitor, &opts.walk, progress)?;

    let mut findings: Vec<Finding> = classify(&out, &visitor).iter().map(|c| finding(c, env.now(), opts)).collect();
    findings
        .sort_by(|a, b| a.class.cmp(&b.class).then(Reverse(a.bytes).cmp(&Reverse(b.bytes))).then(a.path.cmp(&b.path)));

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

    Ok(ScanReport {
        schema_version: SCHEMA_VERSION,
        ddc_version: env!("CARGO_PKG_VERSION").to_owned(),
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
    })
}

fn finding(c: &Classified, now: SystemTime, opts: &ScanOptions) -> Finding {
    let min_age = opts.min_age;
    // Activity of the project root and of the artifact's own directory: a
    // fresh manifest next to the artifact counts even under a stale git root.
    let last = c.project_root.as_deref().and_then(|root| {
        let near = c.owner.path.parent().filter(|p| *p != root).and_then(project::last_activity);
        project::last_activity(root).max(near)
    });
    let project = c.project_root.as_ref().map(|root| Project {
        root: root.clone(),
        last_activity: last.map(|t| humantime::format_rfc3339_seconds(t).to_string()),
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
    // ddc's own leftovers of an interrupted clean are never hidden: they
    // would stay on disk for good.
    let ignored =
        c.rule.id != TOMBSTONE && (opts.disabled_rules.iter().any(|r| r == c.rule.id) || c.owner.bytes < opts.min_size);
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
    }
}

fn totals(findings: &[Finding], out: &scan::WalkOutput) -> Totals {
    // Claims that are not findings (app bundles, global packages, protected
    // toolchain versions) still occupy space: report it as outside findings.
    let hidden_bytes: u64 = out
        .owners
        .iter()
        .filter(|o| o.kind == scan::OwnerKind::Claim && !findings.iter().any(|f| f.path == o.path))
        .map(|o| o.bytes)
        .sum();
    let (ignored, shown): (Vec<&Finding>, Vec<&Finding>) = findings.iter().partition(|f| f.ignored);
    let classes = RiskClass::ALL
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
    Totals {
        scanned_bytes: out.total_bytes,
        files: out.roots.iter().map(|r| r.files).sum(),
        dirs: out.roots.iter().map(|r| r.dirs).sum(),
        unowned_bytes: out.unowned_bytes + hidden_bytes,
        classes,
        selected_bytes: selected.clone().map(|f| f.bytes).sum(),
        selected_reclaimable_bytes: selected.map(Finding::reclaimable_bytes).sum(),
        ignored_count: ignored.len() as u64,
        ignored_bytes: ignored.iter().map(|f| f.bytes).sum(),
    }
}

/// TCC.db is readable only with Full Disk Access.
/// Whether this process has Full Disk Access (it can read the TCC database).
pub fn full_disk_access(env: &Env) -> bool {
    fs::File::open(env.home().join("Library/Application Support/com.apple.TCC/TCC.db")).is_ok()
}
