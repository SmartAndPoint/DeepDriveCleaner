//! Every profile's S tree, built in a marked temporary folder and scanned
//! in process, matches its manifest: findings, totals and the default plan.
//! Dropped, it leaves nothing in the temporary folder.
//!
//! Under nextest with the `op-stats` counters (`make nextest`), each profile
//! is also built at half of S, and the work of the scan and plan, and of a
//! `clean --items` preflight, may grow from n to 2n entries no faster than
//! each counter's bound (story "Complexity guard: operation counts and
//! scaling exponents"). A quadratic step grows 4 times, not 2.

use std::process::Command;

use ddc_core::stats::{self, Bound, Counter, Snapshot};
use ddc_loadgen::profiles::Profile;
use ddc_loadgen::{Generated, generate, skip_unported, target, verify};

const S: u64 = 10_000;
/// The n of the ratio; S is its 2n.
const N: u64 = S / 2;
/// What is not the algorithm: fixture rounding, fixed scaffolding, scans
/// cut short in walk order, buffers grown by doubling. At 2n it also sets
/// what is caught: a quadratic part fails the check once it is more than a
/// quarter of its counter at n (2 + 2q > 2.5). One kind of cost per counter
/// keeps a quadratic site the bulk of its own counter.
const EPSILON: f64 = 0.25;
/// Below this a count is noise: a jump from 0 to 3 is not a growth of 3.
const FLOOR: u64 = 64;

fn check(p: Profile) {
    let small = own_process().then(|| {
        let dir = target::Scratch::new().unwrap();
        let g = skip_unported(generate(p, N, 1, dir.path(), None))?;
        // Run once and measured the second time: one-time setup is no growth.
        verify::scan_and_plan(&g.layout, &g.home).unwrap();
        verify::preflight_items(&g.manifest, &g.layout, &g.home).unwrap();
        Some(measured(&g))
    });
    let small = small.flatten();

    let dir = target::Scratch::new().unwrap();
    let Some(g) = skip_unported(generate(p, S, 1, dir.path(), None)) else { return };
    assert_eq!(g.layout.tree.as_os_str().len(), target::ROOT_LEN);
    assert!(!g.manifest.findings.is_empty(), "{}: nothing planted", p.name());
    let (report, plan) = verify::scan_and_plan(&g.layout, &g.home).unwrap();
    let diffs = verify::diffs(&g.manifest, &g.layout, &report, &plan);
    assert!(diffs.is_empty(), "{}: {} differences:\n{}", p.name(), diffs.len(), diffs.join("\n"));
    if let Some(small) = small {
        let large = measured(&g);
        assert!(
            large.scan.get(Counter::DirEntriesRead) > 0,
            "{}: the counters read 0; run with --features ddc-core/op-stats, as `make nextest` does",
            p.name()
        );
        let mut over = grown(p, "scan and plan", (&small.scan, small.entries), (&large.scan, large.entries));
        // No list names a path that is not UTF-8, nor an incomplete finding,
        // and every deep-path-max finding is incomplete.
        let listable = p != Profile::DeepPathMax && !g.manifest.fs_caps.is_some_and(|c| c.invalid_utf8);
        match (&small.preflight, &large.preflight) {
            (Some(s), Some(l)) => over.extend(grown(p, "preflight", (s, small.entries), (l, large.entries))),
            (None, None) if !listable => {}
            (s, l) => panic!(
                "{}: an item list at n: {}, at 2n: {}, expected: {listable}; a profile that gains or loses its list updates `listable`",
                p.name(),
                s.is_some(),
                l.is_some()
            ),
        }
        assert!(over.is_empty(), "{}", over.join("\n"));
    }
    // Locked folders and paths past `PATH_MAX` included, nothing is left behind.
    let path = dir.path().to_owned();
    drop(dir);
    assert!(!path.exists(), "{}: {} is left behind", p.name(), path.display());
}

/// nextest runs each test in its own process: the counters are this test's
/// alone. `cargo test` (the nightly AddressSanitizer job) runs the tests as
/// threads of one process, where they are not, so it only verifies. A nextest
/// mode other than its own process fails rather than skips the checks.
#[allow(clippy::disallowed_methods)] // nextest's own variables, not the user's environment
fn own_process() -> bool {
    match std::env::var_os("NEXTEST_EXECUTION_MODE") {
        Some(m) if m == "process-per-test" => true,
        Some(m) => panic!("nextest runs the tests as {m:?}: the counters are not this test's alone"),
        None => false,
    }
}

struct Measured {
    entries: u64,
    scan: Snapshot,
    preflight: Option<Snapshot>,
}

/// The counters' work for one scan and plan, and for one preflight of the
/// load run's item list.
fn measured(g: &Generated) -> Measured {
    let before = stats::snapshot();
    let scanned = verify::scan_and_plan(&g.layout, &g.home).unwrap();
    let scan = stats::snapshot().delta(&before);
    drop(scanned);
    let before = stats::snapshot();
    let preflight = verify::preflight_items(&g.manifest, &g.layout, &g.home).unwrap();
    let preflight = preflight.map(|_| stats::snapshot().delta(&before));
    Measured { entries: g.manifest.entries, scan, preflight }
}

/// A line per counter that grew faster than its bound.
fn grown(p: Profile, what: &str, (a, n): (&Snapshot, u64), (b, m): (&Snapshot, u64)) -> Vec<String> {
    let r = m as f64 / n as f64;
    Counter::ALL
        .iter()
        .filter_map(|&c| {
            let (bound, why) = c.bound();
            let allowed = match bound {
                Bound::Linear => r,
                Bound::NLogN => r * (m as f64).ln() / (n as f64).ln(),
            };
            let (x, y) = (a.get(c), b.get(c));
            (y as f64 > allowed * (1.0 + EPSILON) * x.max(FLOOR) as f64).then(|| {
                format!(
                    "{} {what}: {} grew {:.1}x ({x} -> {y}) for {r:.2}x the entries ({n} -> {m}); bound {bound:?}: {why}",
                    p.name(),
                    c.name(),
                    y as f64 / x.max(1) as f64,
                )
            })
        })
        .collect()
}

/// The counters' marker symbol is in this binary: the positive control of the
/// `nm` checks that keep it out of a release (`make release-check`).
#[test]
fn the_counters_are_visible_to_nm() {
    if !own_process() {
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let out = Command::new("nm").arg(&exe).output().unwrap();
    assert!(out.status.success(), "nm {}: {}", exe.display(), String::from_utf8_lossy(&out.stderr));
    let symbols = String::from_utf8_lossy(&out.stdout);
    assert!(symbols.contains("_DDC_OP_STATS"), "nm finds no _DDC_OP_STATS in {}", exe.display());
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
fn deep_path_max() {
    check(Profile::DeepPathMax);
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

#[test]
fn system_root() {
    check(Profile::SystemRoot);
}

#[test]
fn mixed() {
    check(Profile::Mixed);
}

#[test]
fn hostile() {
    check(Profile::Hostile);
}
