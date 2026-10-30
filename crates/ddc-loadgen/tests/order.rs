//! The report and the plan do not depend on the walk's thread count or on the
//! order a directory's entries are listed in (story "Coverage floor and
//! property tests for the safety invariants", DDC-005). Each case builds a
//! small tree of a profile at a random seed and scans it, one scan root, on
//! one thread through `StdReader` and on eight through a reader that lists
//! every directory in another order.
//!
//! `hostile` is left out: it changes the tree while it is scanned. Across
//! roots a hard link's bytes go to the first root seen, so the scan has one
//! root. `StdReader` reads no private size, so the bulk reader is compared to
//! it with the private sizes left out.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash as _, Hasher as _};
use std::path::Path;
use std::time::SystemTime;

use ddc_core::env::Env;
use ddc_core::model::ScanReport;
use ddc_core::plan::{Plan, PlanOptions, build_plan};
use ddc_core::report::volumes::SystemStatFs;
use ddc_core::report::{self, ScanOptions};
use ddc_core::rules::Catalog;
use ddc_core::scan::{DirListing, DirReader, Progress, ReadError, ReaderKind, StdReader, Times, WalkOptions};
use ddc_loadgen::profiles::Profile;
use ddc_loadgen::{Generated, generate, skip_unported, target};
use proptest::prelude::*;
use proptest::sample::select;
use serde_json::Value;

/// Small enough for 256 cases in seconds, large enough for every profile's parts.
const ENTRIES: u64 = 150;

/// [`StdReader`] with each listing in an order set by the seed and the
/// directory's path.
struct Shuffled(u64);

impl Shuffled {
    fn shuffle(&self, dir: &Path, mut listing: DirListing) -> DirListing {
        listing.entries.sort_by_cached_key(|e| {
            let mut h = DefaultHasher::new();
            (self.0, dir, &e.name).hash(&mut h);
            h.finish()
        });
        listing
    }
}

impl DirReader for Shuffled {
    fn read(
        &self,
        dir: &Path,
        expect: Option<(u64, u64)>,
        want_private: bool,
        times: Times,
    ) -> Result<DirListing, ReadError> {
        StdReader.read(dir, expect, want_private, times).map(|l| self.shuffle(dir, l))
    }
}

/// One tree and the clock every scan of it reads.
struct Case {
    g: Generated,
    env: Env,
}

impl Case {
    fn scan(&self, threads: usize, reader: ReaderKind, through: Option<&dyn DirReader>) -> (Value, Plan) {
        let walk = WalkOptions { threads: Some(threads), reader, ..WalkOptions::default() };
        let opts = ScanOptions { roots: vec![self.g.layout.tree.clone()], walk, ..ScanOptions::default() };
        let progress = Progress::default();
        let report = match through {
            Some(r) => report::scan_with_reader(&self.env, &opts, &progress, &SystemStatFs, r),
            None => report::scan_with(&self.env, &opts, &progress, &SystemStatFs),
        }
        .unwrap();
        let plan = build_plan(&report, &Catalog::builtin(self.env.home()), &PlanOptions::default());
        (comparable(&report), plan)
    }
}

/// The report without what differs between two scans of one tree: the time
/// it ran at and took, and the free space of its volume.
fn comparable(report: &ScanReport) -> Value {
    let mut v = serde_json::to_value(report).unwrap();
    let o = v.as_object_mut().unwrap();
    for key in ["generated_at", "duration_ms", "volumes"] {
        o.remove(key);
    }
    v
}

/// `v` without the fields that hold a private size (see the module's doc).
fn without_private(mut v: Value) -> Value {
    fn strip(v: &mut Value) {
        match v {
            Value::Object(o) => {
                o.retain(|k, _| !k.contains("private") && k != "selected_reclaimable_bytes");
                o.values_mut().for_each(strip);
            }
            Value::Array(a) => a.iter_mut().for_each(strip),
            _ => {}
        }
    }
    strip(&mut v);
    v
}

fn profiles() -> Vec<Profile> {
    Profile::ALL.into_iter().filter(|p| *p != Profile::Hostile).collect()
}

fn config() -> ProptestConfig {
    let base = ProptestConfig::default();
    let max_shrink_time = if base.max_shrink_time == 0 { 30_000 } else { base.max_shrink_time };
    ProptestConfig { max_shrink_time, ..base }
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn the_same_report_and_plan_for_any_thread_count_and_entry_order(
        profile in select(profiles()),
        seed in 1u64..1_000_000,
        order in any::<u64>(),
    ) {
        if ddc_loadgen::unsupported("the_same_report_and_plan_for_any_thread_count_and_entry_order") {
            return Ok(());
        }
        let dir = target::Scratch::new().unwrap();
        let Some(g) = skip_unported(generate(profile, ENTRIES, seed, dir.path(), None)) else { return Ok(()) };
        let env = Env::with_paths(&g.home, &g.layout.data, &g.layout.data.join("logs"), SystemTime::now()).unwrap();
        let case = Case { g, env };
        let shuffled = Shuffled(order);

        // One scan changes both the thread count and the order: a difference
        // in either shows, and the case stays cheap enough for 256 in the gate.
        let (report, plan) = case.scan(1, ReaderKind::Std, Some(&StdReader));
        let (r, p) = case.scan(8, ReaderKind::Std, Some(&shuffled));
        prop_assert_eq!(&r, &report, "{} seed {}: 8 threads, shuffled", profile.name(), seed);
        prop_assert_eq!(&p, &plan, "{} seed {}: 8 threads, shuffled", profile.name(), seed);
        let (bulk, _) = case.scan(8, ReaderKind::Bulk, None);
        prop_assert_eq!(without_private(bulk), without_private(report), "{} seed {}: bulk", profile.name(), seed);
    }
}
