//! `ddc disk`: what uses the whole startup disk, outside the home folder too
//! (story "Whole disk: what lies outside the home folder"). Read-only: the
//! walk outside the home folder is size-only, nothing in it is a finding,
//! and it takes no ddc lock.

mod apfs;
mod areas;
mod conda;
mod home;
mod model;
pub mod plist;
pub(crate) mod simulators;
pub(crate) mod visitor;

use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub use areas::{AREAS, AreaDef};
pub use model::*;

use crate::env::Env;
use crate::exec::{self, CommandRunner};
use crate::platform::Unsupported;
use crate::report::volumes::StatFs;
use crate::rules::Catalog;
use crate::scan::{self, DirReader, Progress, Tag, WalkOptions, WalkOutput};
use visitor::DiskVisitor;

/// Where the system's folders and tools are. Tests put them all under a
/// fixture root ([`Sources::under`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sources {
    /// The Data volume's root: every area is measured through it.
    pub data_root: PathBuf,
    pub diskutil: PathBuf,
    pub tmutil: PathBuf,
    pub xcode_select: PathBuf,
    /// The real `simctl` of CoreSimulator, not the shim under `xcode-select -p`.
    pub simctl: PathBuf,
    /// The Homebrew area's own `brew`, never the first one on the tool path.
    pub brew: PathBuf,
    /// `/opt` through its firmlink, where the all-users conda installs are
    /// and as conda spells their paths.
    pub opt: PathBuf,
    /// Where simulator runtimes' assets live: a runtime's `parentMountPath`
    /// must lie under it.
    pub assets: PathBuf,
}

const SIMCTL: &str = "Library/Developer/PrivateFrameworks/CoreSimulator.framework/Versions/A/Resources/bin/simctl";

impl Sources {
    pub fn macos() -> Self {
        Self::under(Path::new("/"))
    }

    /// Every path of [`Self::macos`] under `root`.
    pub fn under(root: &Path) -> Self {
        Self {
            data_root: root.join("System/Volumes/Data"),
            diskutil: root.join("usr/sbin/diskutil"),
            tmutil: root.join("usr/bin/tmutil"),
            xcode_select: root.join("usr/bin/xcode-select"),
            simctl: root.join(SIMCTL),
            brew: root.join("opt/homebrew/bin/brew"),
            opt: root.join("opt"),
            assets: root.join("System/Library/AssetsV2"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiskOptions {
    /// Worker threads; `None` = available parallelism.
    pub threads: Option<usize>,
    /// Entries read in each temporary-folders area before the rest of it is
    /// left unread and the area is partial. One `$TMPDIR` can hold ten
    /// million folders.
    pub temp_entry_budget: u64,
    /// The oldest `last-scan.json` the home row is taken from.
    pub max_scan_age: Duration,
    /// The user's skips, canonical: never opened.
    pub user_skips: Vec<PathBuf>,
}

impl Default for DiskOptions {
    fn default() -> Self {
        Self {
            threads: None,
            temp_entry_budget: 1_000_000,
            max_scan_age: Duration::from_secs(24 * 3600),
            user_skips: Vec::new(),
        }
    }
}

pub struct DiskPorts<'a> {
    pub commands: &'a dyn CommandRunner,
    pub statfs: &'a dyn StatFs,
}

/// Measures the disk. A set `cancel` stops the tools not yet run and every
/// folder not yet opened; the report is still complete in shape, with the
/// unfinished areas partial. Refused on a platform that is not supported:
/// its walks fold their errors into partial areas, so without its own gate a
/// refusal would come back as a report.
pub fn measure(
    env: &Env,
    sources: &Sources,
    opts: &DiskOptions,
    ports: &DiskPorts<'_>,
    progress: &Progress,
    cancel: &AtomicBool,
) -> Result<DiskReport, Unsupported> {
    measure_with(env, sources, opts, ports, progress, cancel, None)
}

/// [`measure`] through a given reader, so a test can see every folder opened.
pub fn measure_with(
    env: &Env,
    sources: &Sources,
    opts: &DiskOptions,
    ports: &DiskPorts<'_>,
    progress: &Progress,
    cancel: &AtomicBool,
    reader: Option<&dyn DirReader>,
) -> Result<DiskReport, Unsupported> {
    crate::platform::require_supported()?;
    let started = Instant::now();
    let stopped = || cancel.load(Ordering::SeqCst);

    let container =
        (!stopped()).then(|| apfs::container(ports.commands, &sources.diskutil, ports.statfs, &sources.data_root));
    let container = container.flatten();
    let snapshots = (!stopped())
        .then(|| ports.commands.at(&sources.tmutil).and_then(|t| exec::count_local_snapshots(ports.commands, &t)))
        .flatten()
        .map(|n| n as u64);
    let simulators = (!stopped()).then(|| simulators::simulators(ports.commands, sources)).flatten();
    let last = home::last_scan(env, ports.statfs, &sources.data_root, opts.max_scan_age);

    // The areas that exist, by canonical path; the same folder twice (a
    // firmlink, a symlink) is measured once, for the first area naming it.
    let data_root = sources.data_root.canonicalize().ok();
    let mut paths: Vec<Vec<PathBuf>> = vec![Vec::new(); AREAS.len()];
    let mut seen: Vec<(u64, u64)> = Vec::new();
    if let Some(root) = &data_root {
        for (i, def) in AREAS.iter().enumerate() {
            for path in area_paths(root, def) {
                let Ok(md) = std::fs::metadata(&path) else { continue };
                let Ok(canon) = path.canonicalize() else { continue };
                if !md.is_dir() || seen.contains(&(md.dev(), md.ino())) {
                    continue;
                }
                seen.push((md.dev(), md.ino()));
                paths[i].push(canon);
            }
        }
    }
    let home_tag = AREAS.len() as Tag;
    let mut regions: Vec<(PathBuf, Tag)> =
        paths.iter().enumerate().flat_map(|(i, ps)| ps.iter().map(move |p| (p.clone(), i as Tag))).collect();
    let mut roots: Vec<PathBuf> =
        AREAS.iter().zip(&paths).filter(|(def, _)| def.parent.is_none()).flat_map(|(_, ps)| ps.clone()).collect();
    if last.is_none() {
        roots.push(env.home().to_path_buf());
        regions.push((env.home().to_path_buf(), home_tag));
    }
    let hide = data_root.as_deref().and_then(|root| own_home_in_users(root, env.home()));

    let catalog = Catalog::builtin(env.home());
    let mut budgeted: Vec<bool> = AREAS.iter().map(|d| d.budgeted).collect();
    budgeted.push(false);
    let visitor = DiskVisitor {
        hide,
        data_root: data_root.clone(),
        ..DiskVisitor::new(&catalog, &opts.user_skips, &regions, budgeted, opts.temp_entry_budget, cancel)
    };
    let walk_opts = WalkOptions { threads: opts.threads, ..WalkOptions::default() };
    let out = if roots.is_empty() {
        None
    } else {
        match reader {
            Some(reader) => scan::walk_with_reader(&roots, &visitor, &walk_opts, progress, reader).ok(),
            None => scan::walk(&roots, &visitor, &walk_opts, progress).ok(),
        }
    };
    let cancelled = stopped();
    let conda =
        (!cancelled).then(|| conda::installs(&sources.opt, &catalog, &opts.user_skips, opts.threads, cancel)).flatten();

    let tally = |tag: Tag, n_paths: usize| -> Tally {
        let mut t = Tally::default();
        let mut owners = 0;
        for o in out.iter().flat_map(|o| &o.owners).filter(|o| o.tag == tag) {
            owners += 1;
            t.bytes += o.bytes;
            t.shared_bytes += o.shared_bytes;
            t.files += o.files;
            t.dirs += o.dirs;
            t.unreadable += o.errors;
        }
        let pruned = visitor.pruned[tag as usize].load(Ordering::Relaxed) > 0;
        t.partial = pruned || ((cancelled || out.is_none()) && owners < n_paths);
        t
    };

    let areas: Vec<Area> = AREAS
        .iter()
        .enumerate()
        .filter(|(i, _)| !paths[*i].is_empty())
        .map(|(i, def)| {
            let t = tally(i as Tag, paths[i].len());
            Area {
                id: def.id.to_owned(),
                paths: paths[i].clone(),
                parent: def.parent.map(str::to_owned),
                title: def.title.to_owned(),
                explanation: def.explanation.to_owned(),
                frees: def.frees.to_owned(),
                action: def.action,
                reveal: def.reveal,
                bytes: t.bytes,
                shared_bytes: t.shared_bytes,
                files: t.files,
                dirs: t.dirs,
                unreadable: t.unreadable,
                partial: t.partial,
                entries_read: def.budgeted.then(|| visitor.entries[i].load(Ordering::Relaxed)),
                stale: false,
            }
        })
        .collect();

    let generated_at = humantime::format_rfc3339_seconds(env.now()).to_string();
    let walked = out.as_ref().map_or(0, |o| o.total_bytes);
    let home = match &last {
        Some(scan) => HomeRow {
            path: env.home().to_path_buf(),
            source: HomeSource::LastScan,
            as_of: scan.generated_at.clone(),
            bytes: scan.bytes,
            reclaimable_bytes: Some(scan.reclaimable_bytes),
            unreadable: scan.unreadable,
            partial: false,
        },
        None => {
            let t = tally(home_tag, 1);
            HomeRow {
                path: env.home().to_path_buf(),
                source: HomeSource::Walk,
                as_of: generated_at.clone(),
                bytes: t.bytes + t.shared_bytes,
                reclaimable_bytes: None,
                unreadable: t.unreadable,
                partial: t.partial,
            }
        }
    };

    let counted = last.as_ref().map_or(0, |s| s.bytes) + walked;
    let data_volume_bytes =
        container.as_ref().and_then(|c| c.volumes.iter().find(|v| v.role == "data")).and_then(|v| v.used_bytes);
    let not_counted_bytes = data_volume_bytes.map(|d| i64::try_from(d).unwrap_or(i64::MAX) - signed(counted));
    let inconsistent = not_counted_bytes.is_some_and(|b| b < 0);
    let causes = if !inconsistent {
        Vec::new()
    } else if last.is_some() {
        vec![InconsistentCause::StaleHomeRow]
    } else {
        vec![InconsistentCause::ClonesCountedTwice]
    };
    let mut holds: Vec<Held> = catalog
        .no_traverse_paths()
        .into_iter()
        .filter(|p| p.exists())
        .map(|p| Held { kind: HeldKind::Cloud, path: p.to_path_buf() })
        .collect();
    holds.extend(opts.user_skips.iter().map(|p| Held { kind: HeldKind::Skip, path: p.clone() }));

    Ok(DiskReport {
        schema_version: DISK_SCHEMA_VERSION,
        ddc_version: env!("CARGO_PKG_VERSION").to_owned(),
        platform: crate::platform::PLATFORM,
        capabilities: crate::platform::CAPABILITIES.to_vec(),
        generated_at,
        duration_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        cancelled,
        container,
        home,
        areas,
        simulators,
        conda,
        snapshots: Snapshots { count: snapshots },
        walk: walk_totals(out.as_ref()),
        not_counted: NotCounted {
            bytes: not_counted_bytes,
            data_volume_bytes,
            counted_bytes: counted,
            inconsistent,
            causes,
            holds,
            unreadable: out.as_ref().map_or(0, |o| o.errors_total),
        },
    })
}

#[derive(Debug, Default)]
struct Tally {
    bytes: u64,
    shared_bytes: u64,
    files: u64,
    dirs: u64,
    unreadable: u64,
    partial: bool,
}

fn signed(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

fn walk_totals(out: Option<&WalkOutput>) -> WalkTotals {
    out.map_or_else(WalkTotals::default, |o| WalkTotals {
        files: o.roots.iter().map(|r| r.files).sum(),
        dirs: o.roots.iter().map(|r| r.dirs).sum(),
        bytes: o.total_bytes,
    })
}

/// The area's folders under the Data root, before checking they exist.
fn area_paths(root: &Path, def: &AreaDef) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = def.paths.iter().map(|p| root.join(p)).collect();
    if let Some((parent, suffix)) = def.children {
        let mut children: Vec<PathBuf> = crate::stats::read_dir(root.join(parent))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(suffix))
            .map(|e| e.path())
            .collect();
        children.sort();
        out.extend(children);
    }
    out
}

/// The user's home folder as `<data root>/Users/<name>`, when it lies there:
/// the `users` area leaves it to the home row.
fn own_home_in_users(data_root: &Path, home: &Path) -> Option<PathBuf> {
    let md = std::fs::metadata(home).ok()?;
    crate::stats::read_dir(data_root.join("Users"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.dev() == md.dev() && m.ino() == md.ino()))
        .and_then(|p| p.canonicalize().ok())
}
