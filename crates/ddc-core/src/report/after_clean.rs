//! `last-scan.json` after a clean (story "Faster clean: re-check only what
//! the selection needs", gate decisions 5 and 6): the report the CLI read
//! before the clean, edited by what the clean did, instead of a rescan.
//!
//! For what the clean removed, the edit subtracts the report's own figures,
//! never the re-check's, and every subtraction stops at zero. A listed item
//! the clean did not touch (skipped, or not run) takes the re-check's own
//! finding, which the pruned walk measured whole (story "Review window:
//! after a clean, only skips that can clear stay selected"): the report must
//! not keep an identity the next clean skips again as changed since the
//! scan. An item the report does not hold edits nothing. The report drifts
//! from the disk in a few figures until the next full scan replaces it: a
//! picked folder's files and directories, `unowned_bytes`, the `modified` of
//! a Trash finding that took trashed items (it grows but keeps its date), and
//! the `large_files`, `inaccessible` and `skipped` lines under a refreshed
//! item, which leave and come back with the next full scan.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use super::{ScanOptions, count_findings, hidden, sort_findings};
use crate::exec::report::CleanReport;
use crate::exec::{Outcome, SkipReason, VolumeSpace};
use crate::model::{FolderEntryKind, ScanReport};
use crate::rules::entries::has_entries;
use crate::rules::{Catalog, RuleVisitor};
use crate::scan::{self, DirCtx, Pre, Progress, Tag, Visitor};

/// The rule of the home's Trash (`~/.Trash`, `~/.local/share/Trash/files`),
/// where trashed items on its volume go.
const TRASH_RULE: &str = "trash";

/// A tool-cleaned item, measured again after the clean.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remeasured {
    /// The tool removed it: it counts as deleted.
    Gone,
    Measured(Measure),
    /// It could not be measured: the report keeps it as it was.
    Unknown,
}

/// What the scan measures of a finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Measure {
    pub bytes: u64,
    pub shared_bytes: u64,
    pub private_bytes: Option<u64>,
    pub files: u64,
    pub dirs: u64,
    pub incomplete: bool,
    /// The newest modification time inside, Unix seconds, as the scan reads
    /// it ([`scan::OwnerResult::modified`]); a file's own. The report's
    /// RFC 3339 text is made where it is written into a finding.
    pub modified: Option<i64>,
}

/// Measures `path` as the scan measures a finding of `rule`: the whole
/// subtree claimed at its top, APFS private sizes where the rule expects
/// clones. No rule applies inside, as in the scan.
pub fn remeasure(catalog: &Catalog, rule: &str, path: &Path, opts: &scan::WalkOptions) -> Remeasured {
    let md = match fs::symlink_metadata(path) {
        Ok(md) => md,
        Err(e) if e.kind() == ErrorKind::NotFound => return Remeasured::Gone,
        Err(_) => return Remeasured::Unknown,
    };
    if !md.is_dir() {
        let bytes = crate::platform::meta::alloc_bytes(&md);
        let shared = if crate::platform::meta::link_count(&md) > 1 { bytes } else { 0 };
        return Remeasured::Measured(Measure {
            bytes: bytes - shared,
            shared_bytes: shared,
            private_bytes: None,
            files: 1,
            dirs: 0,
            incomplete: false,
            // As the walk reads it: only with the claim times on.
            modified: opts.claim_times.then(|| crate::platform::meta::mtime(&md)),
        });
    }
    let Some(tag) = catalog.by_id(rule).and_then(|r| catalog.tag_of(r)) else { return Remeasured::Unknown };
    let alone = Alone { rules: RuleVisitor::new(catalog, Vec::new()), tag };
    // A fresh flag: a cancel of the clean does not stop its bookkeeping.
    let out = match scan::walk(&[path.to_path_buf()], &alone, opts, &Progress::default()) {
        Ok(out) => out,
        Err(_) if fs::symlink_metadata(path).is_err_and(|e| e.kind() == ErrorKind::NotFound) => {
            return Remeasured::Gone;
        }
        Err(_) => return Remeasured::Unknown,
    };
    match out.owners.iter().find(|o| o.path == path) {
        Some(o) => Remeasured::Measured(Measure {
            bytes: o.bytes,
            shared_bytes: o.shared_bytes,
            private_bytes: o.private_bytes,
            files: o.files,
            dirs: o.dirs,
            incomplete: o.incomplete(),
            modified: o.modified,
        }),
        None => Remeasured::Unknown,
    }
}

/// The rule visitor's measuring, with the walk's root claimed whole.
struct Alone<'a> {
    rules: RuleVisitor<'a>,
    tag: Tag,
}

impl Visitor for Alone<'_> {
    fn before_read(&self, dir: &DirCtx<'_>) -> Pre {
        if dir.depth == 0 { Pre::Claim(self.tag) } else { Pre::Continue }
    }

    fn wants_private(&self, dir: &DirCtx<'_>, tag: Tag) -> bool {
        self.rules.wants_private(dir, tag)
    }

    fn wants_times(&self, dir: &DirCtx<'_>, tag: Tag) -> bool {
        self.rules.wants_times(dir, tag)
    }
}

/// Edits `report` by what `clean` did (gate decision 6). `fresh` is the
/// re-check's walk when `report` is an older one: a skipped or not-run item
/// takes its finding from there. `remeasured` holds the tool-cleaned items;
/// `opts` decides `ignored` for a finding whose size changed.
pub fn edit(
    report: &mut ScanReport,
    clean: &CleanReport,
    fresh: Option<&ScanReport>,
    remeasured: &HashMap<PathBuf, Remeasured>,
    opts: &ScanOptions,
) {
    let mut edit = Edit { report, opts, removed: HashSet::new(), lines: Vec::new() };
    for item in &clean.items {
        match &item.outcome {
            Outcome::Deleted => edit.remove(&item.path, false, item.bytes),
            Outcome::Trashed => edit.remove(&item.path, true, item.bytes),
            Outcome::Native { .. } => match remeasured.get(&item.path) {
                Some(Remeasured::Gone) => edit.remove(&item.path, false, item.bytes),
                Some(Remeasured::Measured(m)) => edit.resize(&item.path, m),
                Some(Remeasured::Unknown) | None => {}
            },
            Outcome::Skipped(reason) => edit.refresh(&item.path, fresh, gone_from_disk(reason)),
            Outcome::NotRun => edit.refresh(&item.path, fresh, false),
            // A failed action may have removed part of it: the next scan measures it.
            Outcome::Failed { .. } => {}
        }
    }
    edit.finish(&clean.volumes);
}

/// Edits `report` for findings a tool changed outside a clean: `ddc disk
/// free`'s `brew cleanup` and the `cache.homebrew` finding (safety ADR,
/// amendment of 2026-10-28). As [`edit`] does for a tool-cleaned item.
pub fn edit_tool_cleaned(report: &mut ScanReport, remeasured: &HashMap<PathBuf, Remeasured>, opts: &ScanOptions) {
    let mut edit = Edit { report, opts, removed: HashSet::new(), lines: Vec::new() };
    for (path, r) in remeasured {
        match r {
            Remeasured::Gone => edit.remove(path, false, None),
            Remeasured::Measured(m) => edit.resize(path, m),
            Remeasured::Unknown => {}
        }
    }
    edit.finish(&[]);
}

/// A skip whose finding is not on the disk as scanned: with no finding at
/// its path in the re-check, it leaves the report.
fn gone_from_disk(reason: &SkipReason) -> bool {
    matches!(
        reason,
        SkipReason::Missing
            | SkipReason::ChangedSinceScan
            | SkipReason::RuleMismatch
            | SkipReason::NotADirectory
            | SkipReason::Symlink
    )
}

struct Edit<'a> {
    report: &'a mut ScanReport,
    opts: &'a ScanOptions,
    /// Indexes of the findings that leave.
    removed: HashSet<usize>,
    /// Paths whose `large_files`, `inaccessible` and `skipped` lines leave.
    lines: Vec<PathBuf>,
}

impl Edit<'_> {
    fn finding(&self, path: &Path) -> Option<usize> {
        (0..self.report.findings.len()).find(|i| !self.removed.contains(i) && self.report.findings[*i].path == path)
    }

    /// The `trash` finding a trashed item on volume `dev` moves to.
    fn trash_on(&self, dev: u64) -> Option<usize> {
        self.report.findings.iter().position(|f| f.rule == TRASH_RULE && f.dev == dev)
    }

    /// The findings at or under `path` still in the report.
    fn at_or_under(&self, path: &Path) -> Vec<usize> {
        (0..self.report.findings.len())
            .filter(|i| !self.removed.contains(i) && self.report.findings[*i].path.starts_with(path))
            .collect()
    }

    /// A deleted or trashed item: a finding with the findings inside it, or a
    /// picked entry of one of the user's folders. `measured` is the clean's
    /// figure, used only for an entry in a folder's rest.
    fn remove(&mut self, path: &Path, trashed: bool, measured: Option<u64>) {
        if let Some(i) = self.finding(path) {
            let dev = self.report.findings[i].dev;
            let trash = if trashed { self.trash_on(dev) } else { None };
            for j in self.at_or_under(path) {
                let f = self.report.findings[j].clone();
                match trash {
                    Some(t) if t != j => {
                        let t = &mut self.report.findings[t];
                        t.bytes = t.bytes.saturating_add(f.bytes);
                        t.files = t.files.saturating_add(f.files);
                        t.dirs = t.dirs.saturating_add(f.dirs);
                    }
                    _ => self.drop_scanned(f.bytes, f.files, f.dirs),
                }
                self.leave_entry(&f.path, f.bytes, None);
                self.removed.insert(j);
            }
            if let Some(t) = trash {
                self.resized(t);
            }
            self.lines.push(path.to_path_buf());
        } else if let Some(folder) = self.folder_of(path) {
            self.remove_entry(folder, path, trashed, measured);
        }
    }

    /// The finding of the folder that `path` is a direct entry of: the
    /// user's own, or a coding agent's session folder, whose entry is keyed
    /// by its transcript and holds its companions' bytes.
    fn folder_of(&self, path: &Path) -> Option<usize> {
        let parent = path.parent()?;
        let i = self.finding(parent)?;
        let f = &self.report.findings[i];
        (has_entries(&f.rule) && f.contents.is_some()).then_some(i)
    }

    /// A picked entry leaves its folder's contents, the findings inside it
    /// with it. Its folder's files and directories are not known, so the
    /// scanned totals keep them; a listed file is one file.
    fn remove_entry(&mut self, folder: usize, path: &Path, trashed: bool, measured: Option<u64>) {
        let Some(contents) = self.report.findings[folder].contents.as_mut() else { return };
        let (bytes, files) = match contents.entries.iter().position(|e| e.path == path) {
            Some(k) => {
                let entry = contents.entries.remove(k);
                (entry.bytes, u64::from(entry.kind == FolderEntryKind::File))
            }
            // In the rest: the clean's measure, and its kind is not known either.
            None => {
                let bytes = measured.unwrap_or(0);
                contents.rest_count = contents.rest_count.saturating_sub(1);
                contents.rest_bytes = contents.rest_bytes.saturating_sub(bytes);
                (bytes, 0)
            }
        };
        let inside = self.at_or_under(path);
        let inner: u64 = inside.iter().map(|&j| self.report.findings[j].bytes).sum();
        self.removed.extend(inside);
        let f = &mut self.report.findings[folder];
        // The folder never counted the findings inside; the entry's bytes hold them.
        f.bytes = f.bytes.saturating_sub(bytes.saturating_sub(inner));
        let dev = f.dev;
        self.resized(folder);
        match if trashed { self.trash_on(dev) } else { None } {
            Some(t) => {
                let t_finding = &mut self.report.findings[t];
                t_finding.bytes = t_finding.bytes.saturating_add(bytes);
                t_finding.files = t_finding.files.saturating_add(files);
                self.resized(t);
            }
            None => self.drop_scanned(bytes, files, 0),
        }
        self.lines.push(path.to_path_buf());
    }

    /// A finding inside a listed entry of a user's folder (a project in
    /// Downloads) shrinks that entry, or the folder's rest. `measure` is its
    /// new size, `None` when it left.
    fn leave_entry(&mut self, path: &Path, bytes: u64, measure: Option<u64>) {
        for f in self.report.findings.iter_mut().filter(|f| f.path != path && path.starts_with(&f.path)) {
            let Some(contents) = f.contents.as_mut() else { continue };
            let Some(name) = path.strip_prefix(&f.path).ok().and_then(|rel| rel.components().next()) else {
                continue;
            };
            let child = f.path.join(name);
            if child == path {
                // A direct child that is a finding is no entry and not in the rest.
                continue;
            }
            match contents.entries.iter_mut().find(|e| e.path == child) {
                Some(entry) => {
                    entry.bytes = entry.bytes.saturating_sub(bytes).saturating_add(measure.unwrap_or(0));
                    if measure.is_none() {
                        entry.findings = entry.findings.saturating_sub(1);
                    }
                }
                None => {
                    contents.rest_bytes = contents.rest_bytes.saturating_sub(bytes).saturating_add(measure.unwrap_or(0))
                }
            }
        }
    }

    /// An item the clean did not touch, as the re-check found it. A picked
    /// entry, or a finding inside another listed item (`inside-another-item`,
    /// which left with it), is not edited.
    fn refresh(&mut self, path: &Path, fresh: Option<&ScanReport>, drop_if_absent: bool) {
        let Some(fresh) = fresh else { return };
        let Some(i) = self.finding(path) else { return };
        if self.report.findings[i].contents.is_some() {
            return;
        }
        match fresh.findings.iter().find(|f| f.path == path) {
            Some(new) => {
                let new = new.clone();
                let m = Measure {
                    bytes: new.bytes,
                    shared_bytes: new.shared_bytes,
                    private_bytes: new.private_bytes,
                    files: new.files,
                    dirs: new.dirs,
                    incomplete: new.incomplete,
                    // The finding is replaced whole below, its date included.
                    modified: None,
                };
                self.resize(path, &m);
                self.report.findings[i] = new;
                self.resized(i);
                self.lines.push(path.to_path_buf());
            }
            None if drop_if_absent => {
                for j in self.at_or_under(path) {
                    let f = self.report.findings[j].clone();
                    self.drop_scanned(f.bytes, f.files, f.dirs);
                    self.leave_entry(&f.path, f.bytes, None);
                    self.removed.insert(j);
                }
                self.lines.push(path.to_path_buf());
            }
            None => {}
        }
    }

    /// A tool-cleaned item at its new size.
    fn resize(&mut self, path: &Path, m: &Measure) {
        let Some(i) = self.finding(path) else { return };
        let old = self.report.findings[i].clone();
        let totals = &mut self.report.totals;
        totals.scanned_bytes = totals.scanned_bytes.saturating_sub(old.bytes).saturating_add(m.bytes);
        totals.files = totals.files.saturating_sub(old.files).saturating_add(m.files);
        totals.dirs = totals.dirs.saturating_sub(old.dirs).saturating_add(m.dirs);
        self.leave_entry(path, old.bytes, Some(m.bytes));
        let f = &mut self.report.findings[i];
        f.bytes = m.bytes;
        f.shared_bytes = m.shared_bytes;
        f.private_bytes = m.private_bytes;
        f.files = m.files;
        f.dirs = m.dirs;
        f.incomplete = m.incomplete;
        f.modified = m.modified.and_then(super::rfc3339_secs);
        self.resized(i);
    }

    /// A finding whose size changed: `ignored` and `selected` follow it, as
    /// the scan decides them.
    fn resized(&mut self, i: usize) {
        let f = &mut self.report.findings[i];
        f.private_bytes = f.private_bytes.map(|p| p.min(f.bytes));
        f.ignored = hidden(&f.rule, f.bytes, self.opts);
        f.selected = f.not_selected.is_none() && !f.ignored;
    }

    fn drop_scanned(&mut self, bytes: u64, files: u64, dirs: u64) {
        let totals = &mut self.report.totals;
        totals.scanned_bytes = totals.scanned_bytes.saturating_sub(bytes);
        totals.files = totals.files.saturating_sub(files);
        totals.dirs = totals.dirs.saturating_sub(dirs);
    }

    fn finish(self, volumes: &[VolumeSpace]) {
        let Edit { report, removed, lines, .. } = self;
        let mut i = 0;
        report.findings.retain(|_| {
            i += 1;
            !removed.contains(&(i - 1))
        });
        let gone = |p: &Path| lines.iter().any(|l| p.starts_with(l));
        report.large_files.retain(|l| !gone(&l.path));
        let before = report.inaccessible.len();
        report.inaccessible.retain(|l| !gone(&l.path));
        report.errors_total = report.errors_total.saturating_sub((before - report.inaccessible.len()) as u64);
        let before = report.skipped.len();
        report.skipped.retain(|l| !gone(&l.path));
        report.skipped_total = report.skipped_total.saturating_sub((before - report.skipped.len()) as u64);
        for space in volumes {
            if let Some(v) = report.volumes.iter_mut().find(|v| v.mount_point == space.mount_point) {
                v.free_bytes = space.free_after;
            }
        }
        sort_findings(&mut report.findings);
        count_findings(&mut report.totals, &report.findings);
    }
}
