//! `ddc-loadgen slopes` (story "Complexity guard: operation counts and
//! scaling exponents"): the load tables of one profile set at n, 2n, 4n and
//! 8n entries, fitted as log-log slopes of the median CPU time and peak
//! memory per `(profile, command)`. A linear command has slope 1; above
//! [`MAX_SLOPE`] it fails in `enforce` mode.
//!
//! The judged slope is the local one between the two largest sizes: the
//! constant base of every command flattens a fit through all four. CPU is
//! judged for [`JUDGED`] commands on a quiet machine, from [`limits::CPU_RUNS`]
//! runs, and only where the median at 4n is at least [`CPU_FLOOR_MS`]: a
//! spread of δ ms on a median of m ms at 4n moves the slope by about
//! δ / (m · ln 2), so the tens of milliseconds a scan of a few hundred wobbles by move it by
//! more than 0.1, and the 3 % a second-long one does by about 0.04. In
//! `record` mode nothing is judged. Peak memory is judged
//! above the profile's base from `bench/load-limits.tsv`, and only at L or
//! XL: at M most profiles add a few hundred bytes per entry to an 11 MB
//! base, so the raw slope is near 0 whatever the algorithm.

use std::collections::BTreeMap;

use crate::limits::{self, Limits, Mode};
use crate::table::{self, Meta, Row};

pub const MAX_SLOPE: f64 = 1.2;
pub const CPU_FLOOR_MS: f64 = 1_000.0;
/// The commands judged; the others are recorded.
pub const JUDGED: [&str; 2] = ["scan", "clean-dry-run"];
/// Sizes whose peak memory is judged.
const MEMORY_SIZES: [&str; 2] = ["L", "XL"];
pub const COLUMNS: [&str; 15] = [
    "profile",
    "command",
    "size",
    "points",
    "entries",
    "cpu_ms",
    "cpu_slope",
    "cpu_fit",
    "cpu_judged",
    "rss_base_bytes",
    "rss_bytes",
    "rss_slope",
    "rss_fit",
    "rss_judged",
    "verdict",
];

/// One size's medians.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Point {
    entries: u64,
    cpu_ms: f64,
    rss: f64,
    runs: usize,
}

#[derive(Debug)]
pub struct Slopes {
    /// The table: a `#` line, the header, a row per `(profile, command)`.
    pub text: String,
    /// A line per judged slope over [`MAX_SLOPE`] or per check that could
    /// not be made; empty in `record` mode.
    pub over: Vec<String>,
}

/// The slopes of `tables`, one per size, judged as `mode` says. `limits`
/// gives the memory bases; a judged command without one fails in `enforce`.
pub fn slopes(tables: &[String], limits: &Limits, mode: Mode) -> anyhow::Result<Slopes> {
    let mut parsed = tables.iter().map(|t| table::parse(t)).collect::<anyhow::Result<Vec<_>>>()?;
    parsed.sort_by_key(|(_, rows)| rows.first().map_or(0, |r| r.entries));
    let (first, last) = match (parsed.first(), parsed.last()) {
        (Some(f), Some(l)) if parsed.len() >= 2 => (&f.0, &l.0),
        _ => anyhow::bail!("slopes need the tables of two sizes or more, not {}", parsed.len()),
    };
    if let Some((m, _)) = parsed.iter().find(|(m, _)| m.seed != first.seed) {
        anyhow::bail!("the tables mix seeds {} and {}", first.seed, m.seed);
    }
    let sizes: Vec<u64> = parsed.iter().map(|(_, rows)| rows.first().map_or(0, |r| r.entries)).collect();
    if let Some(w) = sizes.windows(2).find(|w| w[0] == w[1]) {
        anyhow::bail!("two tables are both at {} entries", w[0]);
    }
    let rows: Vec<&Row> = parsed.iter().flat_map(|(_, r)| r).collect();
    if let Some(r) = rows.iter().find(|r| r.exit != "0") {
        anyhow::bail!("{} on {} at {} entries exited {}: measure again", r.command, r.profile, r.entries, r.exit);
    }
    let size = rows.first().map_or("-", |r| r.size.as_str()).to_owned();
    if let Some(r) = rows.iter().find(|r| r.size != size) {
        anyhow::bail!("the tables mix sizes {size} and {}", r.size);
    }
    // The run's own builds must not decide it: every table carries the load
    // from before the first one.
    let quiet = parsed.iter().all(|(m, _)| limits::cpu_judged(m.load_start, m.ncpu));
    let memory = MEMORY_SIZES.contains(&size.as_str());
    let enforce = mode == Mode::Enforce;

    let mut points: BTreeMap<(String, String), Vec<Point>> = BTreeMap::new();
    for (_, rows) in &parsed {
        for ((command, profile), (entries, rss, cpu_ms, runs)) in limits::medians(rows) {
            points.entry((profile, command)).or_default().push(Point { entries, cpu_ms, rss, runs });
        }
    }

    let mut lines = Vec::new();
    let mut over = Vec::new();
    // A profile that lost a judged command would pass unjudged.
    let profiles: std::collections::BTreeSet<&String> = points.keys().map(|(profile, _)| profile).collect();
    for profile in profiles.into_iter().filter(|_| enforce) {
        for command in JUDGED.iter().filter(|c| !points.contains_key(&(profile.clone(), (**c).to_owned()))) {
            over.push(format!("{command} on {profile}: not measured"));
        }
    }
    for ((profile, command), mut p) in points {
        p.sort_by_key(|p| p.entries);
        let judged = JUDGED.contains(&command.as_str());
        let [.., a, b] = p[..] else {
            if judged {
                anyhow::bail!("{command} on {profile}: measured at one size only");
            }
            // A recorded command keeps its row and leaves the fit to the others.
            let b = p[0];
            let mut cells = vec![profile, command, size.clone(), "1".into(), b.entries.to_string()];
            cells.extend([format!("{:.0}", b.cpu_ms), "-".into(), "-".into(), "no".into(), "-".into()]);
            cells.extend([format!("{:.0}", b.rss), "-".into(), "-".into(), "no".into(), "-".into()]);
            lines.push(cells.join("\t"));
            continue;
        };
        let cpu: Vec<(f64, f64)> = p.iter().map(|p| (p.entries as f64, p.cpu_ms)).collect();
        let cpu_slope = slope(&cpu[cpu.len() - 2..]);
        let cpu_judged = judged
            && enforce
            && quiet
            && a.runs.min(b.runs) >= limits::CPU_RUNS
            && a.cpu_ms >= CPU_FLOOR_MS
            && cpu_slope.is_some();

        let base = limits.get(&(command.clone(), profile.clone())).map(|l| l.rss_base / limits::HEADROOM);
        let above: Option<Vec<(f64, f64)>> =
            base.map(|base| p.iter().map(|p| (p.entries as f64, p.rss - base)).collect());
        let rss_slope = above.as_ref().and_then(|v| slope(&v[v.len() - 2..]));
        let rss_judged = judged && enforce && memory && rss_slope.is_some();
        if judged && enforce && memory && rss_slope.is_none() {
            over.push(match base {
                None => format!("{command} on {profile}: no memory base (re-baseline: ops/profiling-the-core.md)"),
                Some(base) => format!("{command} on {profile}: peak memory at or below its base {base:.0} bytes"),
            });
        }

        let mut bad = Vec::new();
        if let (true, Some(s)) = (cpu_judged, cpu_slope)
            && s > MAX_SLOPE
        {
            bad.push(format!("CPU slope {s:.2} ({:.0} -> {:.0} ms)", a.cpu_ms, b.cpu_ms));
        }
        if let (true, Some(s)) = (rss_judged, rss_slope)
            && s > MAX_SLOPE
        {
            bad.push(format!("peak memory slope {s:.2} above the base ({:.0} -> {:.0} bytes)", a.rss, b.rss));
        }
        let verdict = match (cpu_judged || rss_judged, bad.is_empty()) {
            (false, _) => "-",
            (true, true) => "ok",
            (true, false) => "over",
        };
        if !bad.is_empty() {
            over.push(format!(
                "{command} on {profile}: {} from {} to {} entries; the bound is {MAX_SLOPE}",
                bad.join(", "),
                a.entries,
                b.entries
            ));
        }
        let num = |v: Option<f64>| v.map_or("-".to_owned(), |v| format!("{v:.3}"));
        let yes = |b: bool| if b { "yes" } else { "no" };
        lines.push(
            [
                profile.clone(),
                command.clone(),
                size.clone(),
                p.len().to_string(),
                b.entries.to_string(),
                format!("{:.0}", b.cpu_ms),
                num(cpu_slope),
                num(slope(&cpu)),
                yes(cpu_judged).to_owned(),
                base.map_or("-".to_owned(), |b| format!("{b:.0}")),
                format!("{:.0}", b.rss),
                num(rss_slope),
                num(above.as_deref().and_then(slope)),
                yes(rss_judged).to_owned(),
                verdict.to_owned(),
            ]
            .join("\t"),
        );
    }

    let meta = Meta {
        load_end: last.load_end,
        mode: match (mode, quiet) {
            (Mode::Record, _) => "record",
            (Mode::Enforce, true) => "enforce",
            (Mode::Enforce, false) => "enforce-memory",
        }
        .to_owned(),
        ..first.clone()
    };
    let mut text = format!("{}\n{}\n", meta.line(), COLUMNS.join("\t"));
    for l in &lines {
        text.push_str(l);
        text.push('\n');
    }
    Ok(Slopes { text, over })
}

/// The least-squares slope of ln y against ln x; none when a y is not
/// positive or fewer than two sizes differ.
fn slope(points: &[(f64, f64)]) -> Option<f64> {
    if points.len() < 2 || points.iter().any(|&(x, y)| x <= 0.0 || y <= 0.0) {
        return None;
    }
    let logs: Vec<(f64, f64)> = points.iter().map(|&(x, y)| (x.ln(), y.ln())).collect();
    let n = logs.len() as f64;
    let (mx, my) = (logs.iter().map(|p| p.0).sum::<f64>() / n, logs.iter().map(|p| p.1).sum::<f64>() / n);
    let sxx: f64 = logs.iter().map(|p| (p.0 - mx).powi(2)).sum();
    let sxy: f64 = logs.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    (sxx > 0.0).then(|| sxy / sxx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::Limit;

    fn meta(load_start: f64) -> Meta {
        Meta {
            date: "2026-10-29".into(),
            commit: "abc1234".into(),
            model: "Mac16,8".into(),
            ncpu: 12,
            load_start,
            load_end: load_start,
            mode: "record".into(),
            seed: 1,
        }
    }

    /// A table at `entries` for `mixed`: `runs` runs of each command with the
    /// CPU and peak memory `f` gives for the entries.
    fn table(size: &str, entries: u64, load: f64, runs: u32, f: impl Fn(f64) -> (f64, f64)) -> String {
        commands(&["scan", "clean-dry-run", "scan-background"], size, entries, load, runs, f)
    }

    fn commands(
        commands: &[&str],
        size: &str,
        entries: u64,
        load: f64,
        runs: u32,
        f: impl Fn(f64) -> (f64, f64),
    ) -> String {
        let (cpu, rss) = f(entries as f64);
        let rows: Vec<String> = commands
            .iter()
            .flat_map(|c| (1..=runs).map(move |run| (c, run)))
            .map(|(c, run)| {
                Row {
                    profile: "mixed".into(),
                    size: size.into(),
                    seed: 1,
                    entries,
                    command: (*c).into(),
                    run,
                    wall_ms: 0,
                    user_ms: cpu as u128,
                    sys_ms: 0,
                    cpu_ms: cpu as u128,
                    max_rss_bytes: rss as u64,
                    threads_max: 1,
                    exit: "0".into(),
                }
                .line()
            })
            .collect();
        table::render(&meta(load), &rows)
    }

    fn four(size: &str, tier: u64, load: f64, f: impl Fn(f64) -> (f64, f64) + Copy) -> Vec<String> {
        [8, 4, 2, 1].iter().map(|d| table(size, tier / d, load, 3, f)).collect()
    }

    const BASE: f64 = 11_000_000.0;

    fn limits() -> Limits {
        let l =
            Limit { rss_base: BASE * limits::HEADROOM, rss_per_entry: 0.0, cpu_base_ms: 0.0, cpu_ms_per_entry: 0.0 };
        [(("scan".to_owned(), "mixed".to_owned()), l), (("clean-dry-run".to_owned(), "mixed".to_owned()), l)].into()
    }

    fn row<'a>(s: &'a Slopes, command: &str) -> Vec<&'a str> {
        let line = s.text.lines().find(|l| l.split('\t').nth(1) == Some(command)).unwrap();
        line.split('\t').collect()
    }

    #[test]
    fn a_linear_command_passes_and_a_quadratic_one_fails() {
        let linear = |n: f64| (100.0 + 0.05 * n, BASE + 200.0 * n);
        let s = slopes(&four("L", 1_000_000, 1.0, linear), &limits(), Mode::Enforce).unwrap();
        assert!(s.over.is_empty(), "{:?}", s.over);
        let r = row(&s, "scan");
        assert_eq!((r[3], r[4], r[8], r[13], r[14]), ("4", "1000000", "yes", "yes", "ok"));
        let cpu: f64 = r[6].parse().unwrap();
        assert!((0.95..=1.0).contains(&cpu), "{r:?}");
        // Above the base the memory is exactly linear; the full fit is flattened by the CPU base.
        assert_eq!(r[11], "1.000");
        assert!(r[7].parse::<f64>().unwrap() < cpu);

        let quadratic = |n: f64| (100.0 + 1e-6 * n * n, BASE + 200.0 * n);
        let s = slopes(&four("L", 1_000_000, 1.0, quadratic), &limits(), Mode::Enforce).unwrap();
        assert_eq!(s.over.len(), 2, "{:?}", s.over);
        assert!(s.over[1].starts_with("scan on mixed: CPU slope 2.00"), "{}", s.over[1]);
        assert_eq!(row(&s, "scan")[14], "over");
        // Not a judged command: recorded, never failed.
        assert_eq!(row(&s, "scan-background")[14], "-");
        // Record mode judges nothing, and its table says so.
        let s = slopes(&four("L", 1_000_000, 1.0, quadratic), &limits(), Mode::Record).unwrap();
        assert!(s.over.is_empty(), "{:?}", s.over);
        let r = row(&s, "scan");
        assert_eq!((r[8], r[13], r[14]), ("no", "no", "-"));
    }

    #[test]
    fn cpu_is_judged_only_quiet_from_three_runs_and_past_the_floor() {
        let quadratic = |n: f64| (100.0 + 1e-6 * n * n, BASE + 200.0 * n);
        let busy = slopes(&four("L", 1_000_000, 6.0, quadratic), &limits(), Mode::Enforce).unwrap();
        assert!(busy.over.is_empty(), "{:?}", busy.over);
        assert!(busy.text.starts_with("# ") && busy.text.lines().next().unwrap().contains(" mode=enforce-memory "));
        assert_eq!(row(&busy, "scan")[8], "no");

        let once: Vec<String> = [8, 4, 2, 1].iter().map(|d| table("L", 1_000_000 / d, 1.0, 1, quadratic)).collect();
        assert!(slopes(&once, &limits(), Mode::Enforce).unwrap().over.is_empty());

        // 500 ms at 4n: recorded, not judged.
        let small = |n: f64| (2e-9 * n * n, BASE + 200.0 * n);
        let s = slopes(&four("L", 1_000_000, 1.0, small), &limits(), Mode::Enforce).unwrap();
        assert!(s.over.is_empty(), "{:?}", s.over);
        assert_eq!(row(&s, "scan")[8], "no");
    }

    #[test]
    fn memory_is_judged_above_its_base_and_only_at_l() {
        let growing = |n: f64| (100.0 + 0.05 * n, BASE + 1e-3 * n * n);
        let at_m = slopes(&four("M", 100_000, 1.0, growing), &limits(), Mode::Enforce).unwrap();
        assert!(at_m.over.is_empty(), "{:?}", at_m.over);
        assert_eq!(row(&at_m, "scan")[13], "no");
        let at_l = slopes(&four("L", 1_000_000, 1.0, growing), &limits(), Mode::Enforce).unwrap();
        assert_eq!(at_l.over.len(), 2, "{:?}", at_l.over);
        assert!(at_l.over[1].starts_with("scan on mixed: peak memory slope 2.00"), "{}", at_l.over[1]);
        // A busy machine still judges memory.
        let busy = slopes(&four("L", 1_000_000, 6.0, growing), &limits(), Mode::Enforce).unwrap();
        assert!(busy.text.lines().next().unwrap().contains(" mode=enforce-memory "));
        assert_eq!(busy.over.len(), 2, "{:?}", busy.over);

        // A judged command with no base, or at its base, fails closed at L.
        let linear = |n: f64| (100.0 + 0.05 * n, BASE + 200.0 * n);
        let s = slopes(&four("L", 1_000_000, 1.0, linear), &Limits::new(), Mode::Enforce).unwrap();
        assert_eq!(s.over[1], "scan on mixed: no memory base (re-baseline: ops/profiling-the-core.md)");
        let flat = |n: f64| (100.0 + 0.05 * n, BASE);
        let s = slopes(&four("L", 1_000_000, 1.0, flat), &limits(), Mode::Enforce).unwrap();
        assert_eq!(s.over[1], "scan on mixed: peak memory at or below its base 11000000 bytes");
    }

    #[test]
    fn tables_of_one_size_or_of_mixed_seeds_are_refused() {
        let linear = |n: f64| (100.0 + 0.05 * n, BASE + 200.0 * n);
        assert!(slopes(&[table("L", 1_000_000, 1.0, 3, linear)], &limits(), Mode::Enforce).is_err());
        let mut t = four("L", 1_000_000, 1.0, linear);
        t[0] = t[0].replacen("seed=1", "seed=2", 1);
        assert!(slopes(&t, &limits(), Mode::Enforce).is_err());
        let mut t = four("L", 1_000_000, 1.0, linear);
        t[1] = table("M", 250_000, 1.0, 3, linear);
        assert!(slopes(&t, &limits(), Mode::Enforce).is_err());
        let mut t = four("L", 1_000_000, 1.0, linear);
        t[1] = t[0].clone();
        assert!(slopes(&t, &limits(), Mode::Enforce).unwrap_err().to_string().contains("both at 125000"));
        let mut t = four("L", 1_000_000, 1.0, linear);
        t[2] = t[2].replacen("\t0\n", "\tsig9\n", 1);
        assert!(slopes(&t, &limits(), Mode::Enforce).unwrap_err().to_string().contains("exited sig9"));
    }

    #[test]
    fn a_recorded_command_at_one_size_keeps_its_row_and_a_missing_judged_one_fails() {
        let linear = |n: f64| (100.0 + 0.05 * n, BASE + 200.0 * n);
        let mut t = four("L", 1_000_000, 1.0, linear);
        t[0] = commands(&["scan", "clean-dry-run", "clean-items"], "L", 125_000, 1.0, 3, linear);
        let s = slopes(&t, &limits(), Mode::Enforce).unwrap();
        assert!(s.over.is_empty(), "{:?}", s.over);
        assert_eq!(row(&s, "clean-items")[3..5], ["1", "125000"]);

        let t: Vec<String> = [8, 4, 2, 1]
            .iter()
            .map(|d| commands(&["scan", "scan-background"], "L", 1_000_000 / d, 1.0, 3, linear))
            .collect();
        assert_eq!(slopes(&t, &limits(), Mode::Enforce).unwrap().over, ["clean-dry-run on mixed: not measured"]);
        assert!(slopes(&t, &limits(), Mode::Record).unwrap().over.is_empty());
    }

    #[test]
    fn the_slope_of_a_power_law_is_its_exponent() {
        let pts: Vec<(f64, f64)> = [1.0, 2.0, 4.0, 8.0].iter().map(|&x: &f64| (x, 3.0 * x.powf(1.5))).collect();
        assert!((slope(&pts).unwrap() - 1.5).abs() < 1e-9);
        assert_eq!(slope(&[(1.0, 1.0)]), None);
        assert_eq!(slope(&[(1.0, 1.0), (2.0, 0.0)]), None);
    }
}
