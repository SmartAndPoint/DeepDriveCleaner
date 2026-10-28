//! Scans a built tree in process and lists every difference from its
//! manifest: findings, totals and the default dry-run plan.

use std::collections::BTreeMap;
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::time::SystemTime;

use ddc_core::env::Env;
use ddc_core::plan::{PlanOptions, build_plan};
use ddc_core::report::{self, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::scan::Progress;

use crate::manifest::{Expected, Manifest, Planned, Totals, show};
use crate::target::Layout;

/// The differences, one line each; empty when the scan matches.
pub fn verify(m: &Manifest, layout: &Layout, home: &Path) -> anyhow::Result<Vec<String>> {
    let (totals, found, planned) = scanned(layout, home)?;
    let mut diffs = Vec::new();
    if totals != m.totals {
        diffs.push(format!("totals: expected {:?}, scanned {totals:?}", m.totals));
    }
    compare(&mut diffs, "finding", &m.findings, &found, |f| &f.path);
    compare(&mut diffs, "plan item", &m.plan, &planned, |p| &p.path);
    Ok(diffs)
}

/// For a tree that changes while it is scanned (`hostile`): every finding
/// is one of the manifest's, by path and rule. Sizes and totals may differ.
pub fn subset(m: &Manifest, layout: &Layout, home: &Path) -> anyhow::Result<Vec<String>> {
    let (_, found, _) = scanned(layout, home)?;
    let known: BTreeMap<&String, &String> = m.findings.iter().map(|f| (&f.path, &f.rule)).collect();
    Ok(found
        .iter()
        .filter(|f| known.get(&f.path) != Some(&&f.rule))
        .map(|f| format!("finding not in the manifest: {} ({})", f.path, f.rule))
        .collect())
}

/// One scan and the default plan, in the manifest's terms.
fn scanned(layout: &Layout, home: &Path) -> anyhow::Result<(Totals, Vec<Expected>, Vec<Planned>)> {
    let env = Env::with_paths(home, &layout.data, &layout.data.join("logs"), SystemTime::now())?;
    let opts = ScanOptions { roots: vec![layout.tree.clone()], ..ScanOptions::default() };
    let report = report::scan(&env, &opts, &Progress::default())?;
    let plan = build_plan(&report, &Catalog::builtin(env.home()), &PlanOptions::default());
    let rel = |p: &Path| show(p.strip_prefix(&layout.tree).unwrap_or(p).as_os_str().as_bytes());

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
    Ok((totals, found, planned))
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
