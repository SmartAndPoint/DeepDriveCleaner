//! `ddc agent footprint`: one project's checkouts, what each holds, and
//! what can go, in seconds (story "ddc footprint: a scoped project report";
//! ADR "Agent footprint", §4 and §7). It walks only the main checkout and
//! its worktrees, takes no lock, writes nothing and removes nothing.

mod model;
pub mod scope;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

pub use model::*;
pub use scope::{Scope, resolve_main};

use crate::class::RiskClass;
use crate::env::Env;
use crate::model::{Finding, NotSelected, WorktreeProbe};
use crate::platform::Canonical as _;
use crate::platform::{self, Capability};
use crate::report::{self, ScanError, ScanOptions};
use crate::rules::worktree::{self, Held, Recognized};
use crate::scan::{DirReader, Progress, SkipReason};
use crate::store;

/// Why `ddc agent footprint` cannot report: exit 2, with the error report
/// under `--json`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Refused {
    #[error("{} is not a folder inside a git repository", root.display())]
    NotARepository { root: PathBuf },
    #[error("the main checkout is your home folder ({}); run `ddc scan`", main.display())]
    HomeIsTheCheckout { main: PathBuf },
}

/// The walk of a [`Scope`]: the findings inside its checkouts, as `ddc
/// scan` classifies them, with the worktrees' reasons.
pub struct Scoped {
    pub scope: Scope,
    /// Inside a checkout only, each with the index of its innermost one.
    pub findings: Vec<(usize, Finding)>,
    /// Why each `git.worktree` finding is not selected, by path.
    pub held: HashMap<PathBuf, Vec<Held>>,
    pub(crate) worktrees: HashMap<PathBuf, Recognized>,
    pub(crate) skipped: Vec<(PathBuf, SkipReason)>,
    pub cancelled: bool,
    started: Instant,
}

/// `opts` with the scope's walk roots: settings' skips, excludes and
/// disabled rules apply as in a scan.
pub fn scan_options(scope: &Scope, mut opts: ScanOptions) -> ScanOptions {
    opts.roots = scope.walk_roots();
    opts
}

/// Walks the scope's checkouts from their parents, pruned to them, through
/// the scan's own walk and classification; worktrees that fail only
/// condition 2 or 3 are probed too. `opts` comes from [`scan_options`].
pub fn scoped(
    env: &Env,
    mut scope: Scope,
    opts: &ScanOptions,
    progress: &Progress,
    reader: Option<&dyn DirReader>,
) -> Result<Scoped, ScanError> {
    let reach = scope.reach();
    let walked = report::walk_classified(env, opts, progress, reader, Some(&reach), worktree::Assess::Footprint)?;
    // git lists no orphan: the walk finds them inside the checkouts.
    let mut orphans: Vec<&PathBuf> = walked
        .worktrees
        .iter()
        .filter(|(p, r)| matches!(r, Recognized::Orphan { .. }) && scope.checkout_of(p).is_some())
        .map(|(p, _)| p)
        .collect();
    orphans.sort();
    scope.checkouts.extend(orphans.into_iter().cloned());
    let findings = walked
        .findings
        .into_iter()
        // A checkout's parent is read: nothing of it but the checkouts counts.
        .filter_map(|f| scope.checkout_of(&f.path).map(|i| (i, f)))
        .collect();
    Ok(Scoped {
        findings,
        held: walked.held,
        worktrees: walked.worktrees,
        skipped: walked.out.skipped.iter().map(|s| (s.path.clone(), s.reason)).collect(),
        cancelled: walked.out.cancelled,
        started: walked.started,
        scope,
    })
}

/// The report of a walk: each checkout in its bucket, the totals, and the
/// harness data of the last home scan.
pub fn report(env: &Env, scoped: Scoped, opts: &ScanOptions) -> FootprintReport {
    let Scoped { scope, findings, held, worktrees, skipped, started, .. } = scoped;
    let processes = opts.processes.table();
    let in_use_known = platform::CAPABILITIES.contains(&Capability::InUseCheck);
    let skips: Vec<&PathBuf> = opts.skip.iter().chain(&opts.user_skips).collect();

    // The checkouts' own findings, and what lies in each.
    let mut own: HashMap<usize, Finding> = HashMap::new();
    let mut inside: HashMap<usize, Vec<Finding>> = HashMap::new();
    // A worktree under `<main>` is its own checkout: its findings count in
    // its entry, never in main's.
    for (i, f) in findings {
        let main = i == 0 && !scope.bare;
        match (f.path == scope.checkouts[i], main) {
            (true, false) => drop(own.insert(i, f)),
            // The main checkout's folder is never an item of its own.
            (true, true) => {}
            (false, _) => inside.entry(i).or_default().push(f),
        }
    }

    let mut checkouts = Vec::new();
    let mut not_scanned: Vec<NotScanned> =
        scope.missing.iter().map(|p| NotScanned { path: p.clone(), reason: NotScannedReason::Missing }).collect();
    let mut now_paths: Vec<PathBuf> = Vec::new();
    for (i, path) in scope.checkouts.iter().enumerate() {
        let is_main = i == 0 && !scope.bare;
        let f = own.get(&i);
        if !is_main && f.is_none() && !skips.iter().any(|s| path.starts_with(s)) && kept_by(path, opts) {
            // Under an exclude or a `.ddc-keep` above it: a claim, not a region.
            checkouts.push(CheckoutEntry {
                path: path.clone(),
                kind: CheckoutKind::Linked,
                origin: None,
                worktree: None,
                lock: None,
                bucket: Some(Bucket::HeldBack),
                reason: Some(HeldReason::Kept),
                bytes: 0,
                hint: String::new(),
                findings: inside.remove(&i).unwrap_or_default(),
            });
            continue;
        }
        if !is_main && f.is_none() {
            let reason = if skips.iter().any(|s| path.starts_with(s)) {
                NotScannedReason::Skipped
            } else if skipped.iter().any(|(p, r)| path.starts_with(p) && *r == SkipReason::OtherFilesystem) {
                NotScannedReason::OtherFilesystem
            } else {
                NotScannedReason::NotRecognized
            };
            not_scanned.push(NotScanned { path: path.clone(), reason });
            continue;
        }
        let mut worktree = f.and_then(|f| f.worktree.clone());
        if let Some(state) = worktree.as_mut().filter(|_| !in_use_known) {
            state.in_use = None;
        }
        let lock = match (worktrees.get(path), worktree.as_ref()) {
            (Some(Recognized::Linked(linked)), Some(state)) if state.locked == Some(true) => {
                let pid = worktree::lock_reason(linked).as_deref().and_then(worktree::lock_pid);
                let live = pid.filter(|_| in_use_known).map(|pid| {
                    i32::try_from(pid)
                        .ok()
                        .and_then(|pid| processes.process_start(pid))
                        .is_some_and(|start| start.uid == platform::procs::uid())
                });
                Some(Lock { pid, live })
            }
            _ => None,
        };
        let (kind, bucket, reason) = if is_main {
            (CheckoutKind::Main, None, None)
        } else {
            let f = f.expect("a linked entry has its finding");
            let (bucket, reason) = judge(f, held.get(path).map_or(&[][..], Vec::as_slice), lock);
            let kind = if f.rule == worktree::ORPHAN_RULE { CheckoutKind::Orphan } else { CheckoutKind::Linked };
            (kind, Some(bucket), reason)
        };
        if bucket == Some(Bucket::Now) {
            now_paths.push(path.clone());
        }
        let mut findings = inside.remove(&i).unwrap_or_default();
        findings.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
        checkouts.push(CheckoutEntry {
            path: path.clone(),
            kind,
            origin: f.and_then(|f| f.origin),
            worktree,
            lock,
            bucket,
            reason,
            bytes: f.map_or(0, |f| f.bytes) + findings.iter().map(|f| f.bytes).sum::<u64>(),
            hint: f.filter(|_| !is_main).map(|f| f.hint.clone()).unwrap_or_default(),
            findings,
        });
    }
    // The main checkout first, then the worktrees largest first.
    let main_entry = checkouts.iter().position(|c| c.kind == CheckoutKind::Main).map(|at| checkouts.remove(at));
    checkouts.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    if let Some(main) = main_entry {
        checkouts.insert(0, main);
    }

    let totals = totals(&checkouts, &own, &now_paths);
    let outside_main = checkouts
        .iter()
        .filter(|c| !c.path.starts_with(&scope.main))
        .flat_map(|c| {
            let own = (c.bucket == Some(Bucket::Now)).then(|| c.path.clone());
            let inner = c.findings.iter().filter(|f| f.selected && !f.ignored).map(|f| f.path.clone());
            own.into_iter().chain(if c.bucket == Some(Bucket::Now) { None } else { Some(inner) }.into_iter().flatten())
        })
        .collect();
    let (harness, harness_reason) = match harness(env) {
        Ok(part) => (Some(part), None),
        Err(reason) => (None, Some(reason)),
    };
    FootprintReport {
        schema_version: FOOTPRINT_SCHEMA_VERSION,
        ddc_version: env!("CARGO_PKG_VERSION").to_owned(),
        generated_at: humantime::format_rfc3339_seconds(env.now()).to_string(),
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        root: scope.root.clone(),
        main: scope.main.clone(),
        bare: scope.bare,
        listing_error: scope.listing_error.clone(),
        checkouts,
        not_scanned,
        totals,
        clean_roots: if scope.bare { Vec::new() } else { vec![scope.main.clone()] },
        outside_main,
        harness,
        harness_reason,
        min_age_days: opts.min_age.as_secs() / 86_400,
        worktree_min_age_days: opts.worktree_min_age.as_secs() / 86_400,
    }
}

/// Whether an exclude of the walk lies at or above `path`.
fn kept_by(path: &std::path::Path, opts: &ScanOptions) -> bool {
    opts.excludes.iter().any(|e| path.starts_with(e) || e.canonical().is_ok_and(|e| path.starts_with(e)))
}

/// A worktree's bucket from its finding and the conditions it failed.
fn judge(f: &Finding, held: &[Held], lock: Option<Lock>) -> (Bucket, Option<HeldReason>) {
    let back = |reason| (Bucket::HeldBack, Some(reason));
    if f.rule == worktree::ORPHAN_RULE {
        return back(HeldReason::Orphan);
    }
    if f.rule == crate::rules::KEPT {
        return back(HeldReason::Kept);
    }
    if f.selected && f.class == RiskClass::Rebuildable {
        return (Bucket::Now, None);
    }
    match f.not_selected {
        Some(NotSelected::Immutable) => return back(HeldReason::Immutable),
        Some(NotSelected::ForeignOwner) => return back(HeldReason::ForeignOwner),
        _ => {}
    }
    let transient = |h: &Held| matches!(h, Held::Locked | Held::InUse | Held::Idle);
    if let Some(first) = held.iter().find(|h| !transient(h)) {
        return back(match first {
            Held::NotAHarness(_) => HeldReason::NotAHarness,
            Held::NotLinked => HeldReason::NotLinked,
            Held::Yours => HeldReason::Yours,
            Held::RuleDisabled => HeldReason::RuleDisabled,
            Held::ActivityUnknown => HeldReason::ActivityUnknown,
            Held::Submodules => HeldReason::Submodules,
            Held::Dirty => HeldReason::Dirty,
            Held::IgnoredContent => HeldReason::IgnoredContent,
            Held::OwnCommits => HeldReason::OwnCommits,
            Held::ProbeUnknown | Held::Locked | Held::InUse | Held::Idle => HeldReason::ProbeUnknown,
        });
    }
    if held.is_empty() {
        // Removable by the conditions, yet not selected: hidden by
        // `min_size`, or never judged (no worktree state).
        return back(if f.ignored { HeldReason::HiddenBySettings } else { HeldReason::ProbeUnknown });
    }
    if f.worktree.as_ref().is_none_or(|w| w.probe != WorktreeProbe::Known) {
        return back(HeldReason::ProbeUnknown);
    }
    // A lock naming no pid, or one that does not run, is stale. Without a
    // process table (`live` unknown) it is taken as live: an estimate, never
    // a hint to unlock what a session may hold.
    if held.contains(&Held::Locked) && lock.is_none_or(|l| l.pid.is_none() || l.live == Some(false)) {
        return back(HeldReason::StaleLock);
    }
    (Bucket::AfterSessions, None)
}

fn totals(checkouts: &[CheckoutEntry], own: &HashMap<usize, Finding>, now: &[PathBuf]) -> FootprintTotals {
    let mut t = FootprintTotals::default();
    let mut add = |f: &Finding| {
        let amount = match f.class {
            RiskClass::Safe => &mut t.safe,
            RiskClass::Rebuildable => &mut t.rebuildable,
            RiskClass::Review => &mut t.review,
            RiskClass::UserData => &mut t.user_data,
        };
        amount.count += 1;
        amount.bytes += f.bytes;
    };
    for f in own.values().filter(|f| !f.ignored) {
        add(f);
    }
    for c in checkouts {
        for f in c.findings.iter().filter(|f| !f.ignored) {
            add(f);
        }
    }
    for c in checkouts {
        if c.bucket == Some(Bucket::Now) {
            t.reclaimable_now.rebuildable += c.bytes;
            continue;
        }
        // What is selected inside counts now, and only there.
        let mut selected = 0;
        for f in c.findings.iter().filter(|f| f.selected && !f.ignored && !now.iter().any(|n| f.path.starts_with(n))) {
            match f.class {
                RiskClass::Safe => t.reclaimable_now.safe += f.bytes,
                RiskClass::Rebuildable => t.reclaimable_now.rebuildable += f.bytes,
                RiskClass::Review | RiskClass::UserData => continue,
            }
            selected += f.bytes;
        }
        match c.bucket {
            Some(Bucket::AfterSessions) => t.reclaimable_after_sessions += c.bytes.saturating_sub(selected),
            Some(Bucket::HeldBack) => t.held_back += c.bytes.saturating_sub(selected),
            Some(Bucket::Now) | None => {}
        }
    }
    t.reclaimable_now.bytes = t.reclaimable_now.safe + t.reclaimable_now.rebuildable;
    t
}

/// The `agent.*` findings of the last home scan, or why there are none.
/// `last-scan.json` is replaced atomically, so it is never half written.
fn harness(env: &Env) -> Result<HarnessPart, HarnessReason> {
    let bytes = match std::fs::read(env.data_dir().join(store::LAST_SCAN)) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(HarnessReason::NoReport),
        Err(_) => return Err(HarnessReason::Unreadable),
    };
    let version = serde_json::from_slice::<serde_json::Value>(&bytes)
        .map_err(|_| HarnessReason::Unreadable)?
        .get("schema_version")
        .and_then(serde_json::Value::as_u64);
    if version != Some(u64::from(crate::model::SCHEMA_VERSION)) {
        return Err(HarnessReason::SchemaMismatch);
    }
    let report: crate::model::ScanReport = serde_json::from_slice(&bytes).map_err(|_| HarnessReason::Unreadable)?;
    let written = humantime::parse_rfc3339(&report.generated_at).ok();
    let report_age = written.and_then(|w| env.now().duration_since(w).ok()).map_or(0, |d| d.as_secs());
    let findings = report.findings.into_iter().filter(|f| f.rule.starts_with("agent.")).collect();
    Ok(HarnessPart { report_age, findings })
}
