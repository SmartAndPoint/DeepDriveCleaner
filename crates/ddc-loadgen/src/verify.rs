//! Scans a built tree in process and lists every difference from its
//! manifest: findings, totals and the default dry-run plan.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ddc_core::env::Env;
use ddc_core::exec::preflight::{PreflightPorts, PreflightSummary, preflight};
use ddc_core::exec::{CmdOutput, CommandRunner, OpenVnode, ProcInfo, ProcStart, ProcessTable};
use ddc_core::model::ScanReport;
use ddc_core::plan::{Plan, PlanOptions, build_plan};
use ddc_core::report::{self, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::scan::{Progress, StdReader};

use crate::items;
use crate::manifest::{Expected, Manifest, Planned, Totals, show};
use crate::target::Layout;

/// The differences, one line each; empty when the scan matches.
pub fn verify(m: &Manifest, layout: &Layout, home: &Path) -> anyhow::Result<Vec<String>> {
    let (report, plan) = scan_and_plan(layout, home)?;
    Ok(diffs(m, layout, &report, &plan))
}

/// One scan of the tree and its default plan: the work the complexity guard
/// measures (story "Complexity guard: operation counts and scaling exponents").
pub fn scan_and_plan(layout: &Layout, home: &Path) -> anyhow::Result<(ScanReport, Plan)> {
    let env = env(layout, home)?;
    let opts = ScanOptions { roots: vec![layout.tree.clone()], ..ScanOptions::default() };
    let report = report::scan(&env, &opts, &Progress::default())?;
    let plan = build_plan(&report, &Catalog::builtin(env.home()), &PlanOptions::default());
    Ok((report, plan))
}

/// The differences between a scan and the manifest.
pub fn diffs(m: &Manifest, layout: &Layout, report: &ScanReport, plan: &Plan) -> Vec<String> {
    let m = &with_fs_blocks(m, &layout.tree);
    let (totals, found, planned) = scanned(layout, report, plan);
    let mut diffs = Vec::new();
    if totals != m.totals {
        diffs.push(format!("totals: expected {:?}, scanned {totals:?}", m.totals));
    }
    compare(&mut diffs, "finding", &m.findings, &found, |f| &f.path);
    compare(&mut diffs, "plan item", &m.plan, &planned, |p| &p.path);
    diffs
}

/// The manifest with the blocks of folders and symlinks added. It predicts
/// the files' blocks; a folder's own and a long symlink's, which `du` and the
/// scan count (a few KiB on ext4, none on APFS), depend on the file system,
/// so they are read from the built tree, apart from the scan: every folder
/// on the tree's file system, listed or not (an unreadable one, and one past
/// `PATH_MAX`, is stat'ed through its parent's entry), and the symlinks in
/// the listed ones, never followed. Each goes to the totals and to the
/// deepest expected finding that holds it. A root folder with no blocks of
/// its own says the file system gives folders none: nothing is read.
fn with_fs_blocks(m: &Manifest, tree: &Path) -> Manifest {
    use std::collections::HashMap;

    use ddc_core::platform::meta;
    let mut m = m.clone();
    let Ok(root) = std::fs::symlink_metadata(tree) else { return m };
    let (Ok((dev, _)), true) = (meta::file_id(&root), meta::alloc_bytes(&root) > 0) else { return m };
    let at: HashMap<String, usize> = m.findings.iter().enumerate().map(|(i, f)| (f.path.clone(), i)).collect();
    let mut stack = vec![(tree.to_path_buf(), meta::alloc_bytes(&root))];
    while let Some((dir, own)) = stack.pop() {
        let mut add = |path: &Path, bytes: u64| {
            m.totals.scanned_bytes += bytes;
            let rel = path.strip_prefix(tree).unwrap_or(path);
            if let Some(&i) = rel.ancestors().find_map(|a| at.get(&show(a.as_os_str().as_encoded_bytes()))) {
                m.findings[i].bytes += bytes;
            }
        };
        add(&dir, own);
        let Ok(listing) = std::fs::read_dir(&dir) else { continue };
        for e in listing.flatten() {
            // Through the listing's own descriptor: it reaches past `PATH_MAX`.
            let (Ok(kind), Ok(c)) = (e.file_type(), e.metadata()) else { continue };
            if kind.is_symlink() {
                add(&e.path(), meta::alloc_bytes(&c));
            } else if kind.is_dir() && meta::file_id(&c).is_ok_and(|(d, _)| d == dev) {
                stack.push((e.path(), meta::alloc_bytes(&c)));
            }
        }
    }
    m
}

/// `clean --items --dry-run` in process over the load run's item list
/// (`items::listed`), with no process running and no tool installed; `None`
/// when the tree has no such item or names a list cannot hold.
pub fn preflight_items(m: &Manifest, layout: &Layout, home: &Path) -> anyhow::Result<Option<PreflightSummary>> {
    if m.fs_caps.is_some_and(|c| c.invalid_utf8) {
        return Ok(None);
    }
    let Some(list) = items::listed(m, &layout.tree)? else { return Ok(None) };
    let env = env(layout, home)?;
    let ports = PreflightPorts { commands: &NoTools, procs: &NoProcesses, reader: &StdReader };
    let roots = [layout.tree.clone()];
    let summary = preflight(&env, &Catalog::builtin(env.home()), &roots, &[], &list.items, &ports, &mut |_| Ok(()))?;
    Ok(Some(summary))
}

struct NoTools;

impl CommandRunner for NoTools {
    fn find(&self, _: &str) -> Option<PathBuf> {
        None
    }
    fn run(&self, exe: &Path, _: &[String], _: &[(&str, &str)], _: std::time::Duration) -> std::io::Result<CmdOutput> {
        Err(std::io::Error::other(format!("no tool runs in a preflight: {}", exe.display())))
    }
}

struct NoProcesses;

impl ProcessTable for NoProcesses {
    fn processes(&self) -> Vec<ProcInfo> {
        Vec::new()
    }
    fn open_files(&self, _: i32) -> Vec<PathBuf> {
        Vec::new()
    }
    fn process_start(&self, _: i32) -> Option<ProcStart> {
        None
    }
    fn open_vnodes(&self, _: i32) -> Vec<OpenVnode> {
        Vec::new()
    }
}

fn env(layout: &Layout, home: &Path) -> anyhow::Result<Env> {
    Ok(Env::with_paths(home, &layout.data, &layout.data.join("logs"), SystemTime::now())?)
}

/// For a tree that changes while it is scanned (`hostile`): every finding
/// is one of the manifest's, by path and rule. Sizes and totals may differ.
pub fn subset(m: &Manifest, layout: &Layout, home: &Path) -> anyhow::Result<Vec<String>> {
    let (report, plan) = scan_and_plan(layout, home)?;
    let (_, found, _) = scanned(layout, &report, &plan);
    let known: BTreeMap<&String, &String> = m.findings.iter().map(|f| (&f.path, &f.rule)).collect();
    Ok(found
        .iter()
        .filter(|f| known.get(&f.path) != Some(&&f.rule))
        .map(|f| format!("finding not in the manifest: {} ({})", f.path, f.rule))
        .collect())
}

/// A scan and its plan, in the manifest's terms.
fn scanned(layout: &Layout, report: &ScanReport, plan: &Plan) -> (Totals, Vec<Expected>, Vec<Planned>) {
    let rel = |p: &Path| show(p.strip_prefix(&layout.tree).unwrap_or(p).as_os_str().as_encoded_bytes());

    let totals = Totals {
        files: report.totals.files,
        dirs: report.totals.dirs,
        scanned_bytes: report.totals.scanned_bytes,
        errors: report.errors_total,
    };
    let found: Vec<Expected> = report
        .findings
        .iter()
        .map(|f| Expected {
            path: rel(&f.path),
            rule: f.rule.clone(),
            class: f.class,
            selected: f.selected,
            bytes: f.bytes,
            shared_bytes: f.shared_bytes,
            files: f.files,
            dirs: f.dirs,
            incomplete: f.incomplete,
        })
        .collect();
    let planned: Vec<Planned> = plan
        .items
        .iter()
        .map(|i| Planned {
            path: rel(&i.finding.path),
            rule: i.finding.rule.clone(),
            action: i.action.name().to_owned(),
            selected: i.selected,
        })
        .collect();
    (totals, found, planned)
}

fn compare<'a, T: PartialEq + std::fmt::Debug>(
    diffs: &mut Vec<String>,
    what: &str,
    expected: &'a [T],
    actual: &'a [T],
    key: impl Fn(&'a T) -> &'a String,
) {
    let mut index = |side: &str, items: &'a [T]| {
        let mut map = BTreeMap::new();
        for item in items {
            if map.insert(key(item), item).is_some() {
                diffs.push(format!("{what} listed twice ({side}): {item:?}"));
            }
        }
        map
    };
    let want: BTreeMap<&String, &T> = index("expected", expected);
    let got: BTreeMap<&String, &T> = index("scanned", actual);
    for (path, w) in &want {
        match got.get(path) {
            None => diffs.push(format!("{what} missing: {w:?}")),
            Some(g) if g != w => diffs.push(format!("{what} differs:\n  expected {w:?}\n  scanned  {g:?}")),
            Some(_) => {}
        }
    }
    for (path, g) in &got {
        if !want.contains_key(path) {
            diffs.push(format!("{what} unexpected: {g:?}"));
        }
    }
}
