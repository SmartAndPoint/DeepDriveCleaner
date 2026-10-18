//! Re-check of every plan item right before it is touched (ADR §5): the
//! scan may be seconds or days old.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Component, Path, PathBuf};

use super::SkipReason;
use crate::env::Env;
use crate::plan::PlanItem;
use crate::rules::{Catalog, KEEP_MARKER, PROTECTED_IN_HOME, PROTECTED_SYSTEM, owns};
use crate::sys::fsops::DirFd;
use crate::sys::procs::ProcInfo;

/// A verified target: its parent is open and checked; act through it.
#[derive(Debug)]
pub struct Verified {
    pub parent: DirFd,
    pub name: OsString,
    /// `(dev, ino)` of the finding as checked.
    pub id: (u64, u64),
}

impl Verified {
    /// The same directory is still at the same place (through the parent fd).
    pub fn unchanged(&self) -> bool {
        self.parent.stat_child(&self.name).is_ok_and(|st| !st.is_symlink && st.is_dir && (st.dev, st.ino) == self.id)
    }
}

pub struct Guard<'a> {
    catalog: &'a Catalog,
    roots: Vec<PathBuf>,
    protected_paths: HashSet<String>,
    protected_ids: HashSet<(u64, u64)>,
    /// Directories containing a keep marker inside planned findings.
    markers: Vec<PathBuf>,
    /// Per process inside an app bundle: its pid, the bundle name, and the
    /// executable's own name, which still names a renamed bundle (`Chrome.app`
    /// runs `Google Chrome`, `Xcode16.app` runs `Xcode`).
    apps: Vec<(i32, String, String)>,
    /// Per process: absolute paths it uses, and first components of relative
    /// paths whose base (cwd) is unknown.
    uses: Vec<Uses>,
}

struct Uses {
    pid: i32,
    name: String,
    paths: Vec<PathBuf>,
    unresolved: Vec<OsString>,
}

impl<'a> Guard<'a> {
    pub fn new(
        env: &Env,
        catalog: &'a Catalog,
        roots: &[PathBuf],
        markers: Vec<PathBuf>,
        procs: Vec<ProcInfo>,
    ) -> Self {
        let mut paths: Vec<PathBuf> = PROTECTED_IN_HOME.iter().map(|rel| env.home().join(rel)).collect();
        paths.extend(PROTECTED_SYSTEM.iter().map(PathBuf::from));
        paths.extend(roots.iter().cloned());
        let protected_ids = paths
            .iter()
            .filter_map(|p| fs::symlink_metadata(p).ok())
            .map(|m| {
                use std::os::unix::fs::MetadataExt as _;
                (m.dev(), m.ino())
            })
            .collect();
        Self {
            catalog,
            roots: roots.to_vec(),
            protected_paths: paths.iter().map(|p| fold(p)).collect(),
            protected_ids,
            markers,
            uses: procs.iter().map(uses).collect(),
            apps: procs.iter().filter_map(|p| Some((p.pid, p.app_bundle()?, p.name.clone()))).collect(),
        }
    }

    pub fn check(&self, item: &PlanItem) -> Result<Verified, SkipReason> {
        let f = &item.finding;
        let path = f.path.as_path();
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(SkipReason::Protected);
        };
        if !self.roots.iter().any(|r| path.starts_with(r)) {
            return Err(SkipReason::OutsideScanRoots);
        }
        if self.roots.iter().any(|r| r == path) {
            return Err(SkipReason::ScanRoot);
        }
        if f.incomplete {
            // Unreadable parts or another filesystem mounted inside: never delete blind.
            return Err(SkipReason::Incomplete);
        }
        if self.protected_paths.contains(&fold(path)) {
            return Err(SkipReason::Protected);
        }
        // No symlink anywhere above: the canonical parent is the parent we scanned.
        if parent.canonicalize().ok().as_deref() != Some(parent) {
            return Err(SkipReason::ParentChanged);
        }
        let parent_fd = DirFd::open(parent).map_err(|_| SkipReason::ParentChanged)?;
        let parent_id = parent_fd.identity().map_err(|_| SkipReason::ParentChanged)?;
        if f.parent_dev.zip(f.parent_ino).is_some_and(|id| id != parent_id) {
            return Err(SkipReason::ParentChanged);
        }
        let st = match parent_fd.stat_child(name) {
            Ok(st) => st,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(SkipReason::Missing),
            Err(_) => return Err(SkipReason::ChangedSinceScan),
        };
        if st.is_symlink {
            return Err(SkipReason::Symlink);
        }
        if !st.is_dir {
            return Err(SkipReason::NotADirectory);
        }
        if (st.dev, st.ino) != (f.dev, f.ino) {
            return Err(SkipReason::ChangedSinceScan);
        }
        if self.protected_ids.contains(&(st.dev, st.ino)) {
            return Err(SkipReason::Protected);
        }
        match self.catalog.by_id(&f.rule) {
            Some(rule) if self.catalog.verify(rule, path) => {}
            _ => return Err(SkipReason::RuleMismatch),
        }
        if fs::symlink_metadata(path.join(".git")).is_ok() || f.project.as_ref().is_some_and(|p| p.root == path) {
            return Err(SkipReason::ProjectRoot);
        }
        if self.kept(path) {
            return Err(SkipReason::Kept);
        }
        if let Some(rule) = self.catalog.by_id(&f.rule)
            && let Some((pid, app, _)) =
                self.apps.iter().find(|(_, app, exe)| rule.owners.iter().any(|o| owns(o, app) || owns(o, exe)))
        {
            return Err(SkipReason::OwnerRunning { pid: *pid, name: app.clone() });
        }
        if let Some(reason) = in_use(path, &self.uses) {
            return Err(reason);
        }
        Ok(Verified { parent: parent_fd, name: name.to_os_string(), id: (st.dev, st.ino) })
    }

    fn kept(&self, path: &Path) -> bool {
        if self.markers.iter().any(|m| m.starts_with(path)) {
            return true;
        }
        let root = self.roots.iter().filter(|r| path.starts_with(r)).max_by_key(|r| r.as_os_str().len());
        path.ancestors()
            .take_while(|a| root.is_none_or(|r| a.starts_with(r)))
            .any(|a| fs::symlink_metadata(a.join(KEEP_MARKER)).is_ok())
    }
}

/// APFS is case-insensitive by default: compare protected paths folded.
fn fold(p: &Path) -> String {
    p.to_string_lossy().to_lowercase()
}

/// What a process uses: its executable path (as passed to execve), path-like
/// arguments, working directory and `VIRTUAL_ENV`/`CONDA_PREFIX`. Relative
/// paths are joined with the process's cwd; computed once per process.
fn uses(p: &ProcInfo) -> Uses {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut unresolved = Vec::new();
    for raw in p.exe.iter().map(|e| e.as_os_str()).chain(p.args.iter().map(|a| a.as_os_str())) {
        let path = Path::new(raw);
        if path.is_absolute() {
            paths.push(lexical(path));
        } else if raw.to_string_lossy().contains('/') {
            match &p.cwd {
                Some(cwd) => paths.push(lexical(&cwd.join(path))),
                None => unresolved.extend(
                    path.components().find(|c| matches!(c, Component::Normal(_))).map(|c| c.as_os_str().to_os_string()),
                ),
            }
        }
    }
    paths.extend(p.cwd.iter().cloned());
    paths.extend(p.env_prefixes.iter().cloned());
    let canonical: Vec<PathBuf> = paths.iter().filter_map(|c| canonical_parent(c)).collect();
    paths.extend(canonical);
    Uses { pid: p.pid, name: p.name.clone(), paths, unresolved }
}

/// A process uses `target` if any of its paths lies inside it; with an
/// unknown cwd, a relative path starting with the target's name counts too.
fn in_use(target: &Path, uses: &[Uses]) -> Option<SkipReason> {
    let name = target.file_name()?;
    for u in uses {
        if u.paths.iter().any(|p| p.starts_with(target)) {
            return Some(SkipReason::InUse { pid: u.pid, name: u.name.clone() });
        }
        if u.unresolved.iter().any(|c| c == name) {
            return Some(SkipReason::InUseUnknownCwd { pid: u.pid, name: u.name.clone() });
        }
    }
    None
}

/// Removes `.` components (Path::components keeps a leading one).
fn lexical(p: &Path) -> PathBuf {
    p.components().filter(|c| !matches!(c, Component::CurDir)).collect()
}

/// `/tmp/x/.venv/bin/python` → `/private/tmp/x/.venv/bin/python`: resolve the
/// parent, keep the last component unresolved (it may be a symlink out of the venv).
fn canonical_parent(p: &Path) -> Option<PathBuf> {
    Some(p.parent()?.canonicalize().ok()?.join(p.file_name()?))
}
