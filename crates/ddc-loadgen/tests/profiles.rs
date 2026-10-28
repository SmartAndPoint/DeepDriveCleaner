//! Every profile's S tree, built in a marked temporary folder and scanned
//! in process, matches its manifest: findings, totals and the default plan.

use ddc_loadgen::profiles::Profile;
use ddc_loadgen::{generate, target, verify};

const S: u64 = 10_000;

fn check(p: Profile) {
    let dir = target::marked().unwrap();
    let g = generate(p, S, 1, dir.path(), None).unwrap();
    assert_eq!(g.layout.tree.as_os_str().len(), target::ROOT_LEN);
    assert!(!g.manifest.findings.is_empty(), "{}: nothing planted", p.name());
    let diffs = verify::verify(&g.manifest, &g.layout, &g.home).unwrap();
    assert!(diffs.is_empty(), "{}: {} differences:\n{}", p.name(), diffs.len(), diffs.join("\n"));
}

#[test]
fn projects() {
    check(Profile::Projects);
}

#[test]
fn wide() {
    check(Profile::Wide);
}

#[test]
fn wide_root() {
    check(Profile::WideRoot);
}

#[test]
fn deep() {
    check(Profile::Deep);
}

#[test]
fn users() {
    check(Profile::Users);
}

#[test]
fn links() {
    check(Profile::Links);
}

#[test]
fn names() {
    check(Profile::Names);
}
