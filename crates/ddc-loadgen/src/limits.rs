//! Memory and CPU limits per `(command, profile)` (story "Memory and CPU
//! under load: peak memory, leaks and CPU time"). Each is affine in the
//! entries: `base + k * entries`, with the base from the S run and `k` from
//! the slope between S and M, both measured on the reference machine and
//! set with 25% headroom. The median of the repeated runs is judged, with
//! an absolute slack on top ([`RSS_SLACK`], [`CPU_SLACK_MS`]) so allocator
//! and scheduler noise on a small command cannot trip it; CPU only from
//! [`CPU_RUNS`] runs or more, so the single clean is judged on memory alone.
//! They live in [`PATH`], one file per OS (Linux has no bulk stat and another
//! allocator), written by `ddc-loadgen limits` on that OS.

use std::collections::BTreeMap;

use crate::table::Row;

/// This platform's limits: `bench/load-limits.tsv` for macOS, measured on
/// the reference Mac; `bench/load-limits-linux.tsv` for Linux, measured on
/// a hosted Ubuntu runner (story "Linux scanner and rule catalog", DDC-007).
pub const PATH: &str = match ddc_core::platform::PLATFORM {
    ddc_core::platform::Platform::Linux => "bench/load-limits-linux.tsv",
    _ => "bench/load-limits.tsv",
};

pub const HEADROOM: f64 = 1.25;
pub const RSS_SLACK: f64 = 1_048_576.0;
pub const CPU_SLACK_MS: f64 = 20.0;
pub const CPU_RUNS: usize = 3;
pub const COLUMNS: [&str; 6] =
    ["command", "profile", "rss_base_bytes", "rss_bytes_per_entry", "cpu_base_ms", "cpu_ms_per_entry"];

/// What a run does with the limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Mode {
    /// Writes the numbers and judges nothing (CI, a busy machine).
    Record,
    /// Fails over a limit. CPU falls back to `record` on a busy machine.
    Enforce,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Record => "record",
            Self::Enforce => "enforce",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limit {
    pub rss_base: f64,
    pub rss_per_entry: f64,
    pub cpu_base_ms: f64,
    pub cpu_ms_per_entry: f64,
}

impl Limit {
    pub fn rss(&self, entries: u64) -> f64 {
        self.rss_base + self.rss_per_entry * entries as f64
    }

    pub fn cpu_ms(&self, entries: u64) -> f64 {
        self.cpu_base_ms + self.cpu_ms_per_entry * entries as f64
    }
}

/// `(command, profile)`.
pub type Key = (String, String);

/// Limits by `(command, profile)`.
pub type Limits = BTreeMap<Key, Limit>;

/// Reads `bench/load-limits.tsv`: `#` lines, the header, a row per pair.
pub fn parse(text: &str) -> anyhow::Result<Limits> {
    let mut lines = text.lines().filter(|l| !l.is_empty() && !l.starts_with('#'));
    if lines.next() != Some(COLUMNS.join("\t").as_str()) {
        anyhow::bail!("the limits' header is not {COLUMNS:?}");
    }
    let mut limits = Limits::new();
    for line in lines {
        let c: Vec<&str> = line.split('\t').collect();
        let [command, profile, a, b, d, e] = c[..] else {
            anyhow::bail!("a limits row has {} columns: {line}", c.len())
        };
        let n = |s: &str| s.parse::<f64>().map_err(|_| anyhow::anyhow!("not a number: {s} in {line}"));
        let limit = Limit { rss_base: n(a)?, rss_per_entry: n(b)?, cpu_base_ms: n(d)?, cpu_ms_per_entry: n(e)? };
        limits.insert((command.to_owned(), profile.to_owned()), limit);
    }
    Ok(limits)
}

pub fn render(limits: &Limits, note: &str) -> String {
    let mut out = format!("# {note}\n{}\n", COLUMNS.join("\t"));
    for ((command, profile), l) in limits {
        out.push_str(&format!(
            "{command}\t{profile}\t{:.0}\t{:.3}\t{:.1}\t{:.6}\n",
            l.rss_base, l.rss_per_entry, l.cpu_base_ms, l.cpu_ms_per_entry
        ));
    }
    out
}

/// The medians of one `(command, profile)`'s runs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Median {
    pub entries: u64,
    pub rss: f64,
    /// User + system.
    pub cpu_ms: f64,
    pub user_ms: f64,
    pub runs: usize,
}

/// The median peak memory, CPU time and user time per `(command, profile)`,
/// with the entries and the number of runs.
pub fn medians(rows: &[Row]) -> BTreeMap<Key, Median> {
    let mut runs: BTreeMap<Key, (u64, [Vec<f64>; 3])> = BTreeMap::new();
    for r in rows {
        let e = runs.entry((r.command.clone(), r.profile.clone())).or_insert((r.entries, Default::default()));
        e.1[0].push(r.max_rss_bytes as f64);
        e.1[1].push(r.cpu_ms as f64);
        e.1[2].push(r.user_ms as f64);
    }
    runs.into_iter()
        .map(|(k, (entries, [rss, cpu, user]))| {
            let runs = cpu.len();
            (k, Median { entries, rss: median(rss), cpu_ms: median(cpu), user_ms: median(user), runs })
        })
        .collect()
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    match v.len() {
        0 => 0.0,
        n if n % 2 == 1 => v[n / 2],
        n => (v[n / 2 - 1] + v[n / 2]) / 2.0,
    }
}

/// Limits from a small and a large run of the same commands: the line
/// through the two medians (its slope never below 0), times [`HEADROOM`].
pub fn baseline(small: &[Row], large: &[Row]) -> anyhow::Result<Limits> {
    let (s, l) = (medians(small), medians(large));
    let mut limits = Limits::new();
    for (key, a) in &s {
        let b = l.get(key).ok_or_else(|| anyhow::anyhow!("{key:?}: only in the small run"))?;
        let (n1, rss1, cpu1, n2, rss2, cpu2) = (a.entries, a.rss, a.cpu_ms, b.entries, b.rss, b.cpu_ms);
        if n2 <= n1 {
            anyhow::bail!("{key:?}: the large run has {n2} entries, the small one {n1}");
        }
        let line = |y1: f64, y2: f64| {
            let k = ((y2 - y1) / (n2 - n1) as f64).max(0.0);
            ((y1 - k * n1 as f64).max(0.0) * HEADROOM, k * HEADROOM)
        };
        let (rss_base, rss_per_entry) = line(rss1, rss2);
        let (cpu_base_ms, cpu_ms_per_entry) = line(cpu1, cpu2);
        limits.insert(key.clone(), Limit { rss_base, rss_per_entry, cpu_base_ms, cpu_ms_per_entry });
    }
    if let Some(key) = l.keys().find(|k| !s.contains_key(*k)) {
        anyhow::bail!("{key:?}: only in the large run");
    }
    Ok(limits)
}

/// Whether CPU is judged: the 1-minute load at the start of the whole run
/// is at most a quarter of the CPUs. Memory does not depend on it.
pub fn cpu_judged(load_start: f64, ncpu: usize) -> bool {
    load_start.is_finite() && load_start <= ncpu as f64 / 4.0
}

/// A line per median over its limit; nothing in `record` mode.
pub fn check(limits: &Limits, rows: &[Row], mode: Mode, judge_cpu: bool) -> Vec<String> {
    if mode == Mode::Record {
        return Vec::new();
    }
    let mut over = Vec::new();
    for ((command, profile), Median { entries: n, rss, cpu_ms: cpu, runs, .. }) in medians(rows) {
        let Some(l) = limits.get(&(command.clone(), profile.clone())) else {
            over.push(format!("{command} on {profile}: no limit (re-baseline: ops/profiling-the-core.md)"));
            continue;
        };
        if rss > l.rss(n) + RSS_SLACK {
            over.push(format!("{command} on {profile}: peak memory {rss:.0} bytes over the limit {:.0}", l.rss(n)));
        }
        if judge_cpu && runs >= CPU_RUNS && cpu > l.cpu_ms(n) + CPU_SLACK_MS {
            over.push(format!("{command} on {profile}: CPU {cpu:.0} ms over the limit {:.0}", l.cpu_ms(n)));
        }
    }
    over
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(command: &str, entries: u64, rss: u64, cpu: u128) -> Row {
        Row {
            profile: "mixed".into(),
            size: "-".into(),
            seed: 1,
            entries,
            command: command.into(),
            run: 1,
            wall_ms: 0,
            user_ms: cpu,
            sys_ms: 0,
            cpu_ms: cpu,
            max_rss_bytes: rss,
            threads_max: 1,
            exit: "0".into(),
        }
    }

    /// S: 10 MB and 100 ms; M: 100 MB and 1000 ms, so 1000 bytes and 0.01 ms per entry.
    fn limits() -> Limits {
        let s = [row("scan", 10_000, 10_000_000, 100)];
        let m = [row("scan", 100_000, 100_000_000, 1_000)];
        baseline(&s, &m).unwrap()
    }

    #[test]
    fn the_baseline_is_the_line_through_s_and_m_with_headroom() {
        let l = limits()[&("scan".to_owned(), "mixed".to_owned())];
        assert!((l.rss_per_entry - 1250.0).abs() < 1e-6, "{l:?}");
        assert!(l.rss_base.abs() < 1e-6, "{l:?}");
        assert!((l.rss(100_000) - 125_000_000.0).abs() < 1.0);
        assert!((l.cpu_ms(100_000) - 1250.0).abs() < 1e-6);
        assert_eq!(parse(&render(&limits(), "test")).unwrap(), limits());
    }

    /// The same run three times: enough for CPU to be judged.
    fn three(r: Row) -> Vec<Row> {
        vec![r.clone(), r.clone(), r]
    }

    #[test]
    fn at_the_limit_and_its_slack_passes_and_over_it_fails() {
        let at = three(row("scan", 100_000, 125_000_000 + RSS_SLACK as u64, 1_250 + CPU_SLACK_MS as u128));
        assert!(check(&limits(), &at, Mode::Enforce, true).is_empty());
        let over = three(row("scan", 100_000, 125_000_001 + RSS_SLACK as u64, 1_251 + CPU_SLACK_MS as u128));
        let found = check(&limits(), &over, Mode::Enforce, true);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(check(&limits(), &over, Mode::Record, true).is_empty());
    }

    #[test]
    fn a_single_run_is_judged_on_memory_alone() {
        let once = [row("scan", 100_000, 200_000_000, 9_000)];
        let found = check(&limits(), &once, Mode::Enforce, true);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("peak memory"), "{found:?}");
    }

    #[test]
    fn a_retained_kib_per_entry_fails_the_memory_limit() {
        // The ballast of AC 4: 1 KiB kept per entry, against 1000 bytes measured.
        let ballast = [row("scan", 100_000, 100_000_000 + 1024 * 100_000, 1_000)];
        let found = check(&limits(), &ballast, Mode::Enforce, true);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("peak memory"), "{found:?}");
    }

    #[test]
    fn a_busy_machine_judges_memory_and_not_cpu() {
        assert!(cpu_judged(2.0, 8));
        assert!(!cpu_judged(2.1, 8));
        assert!(!cpu_judged(f64::NAN, 8));
        let slow = three(row("scan", 100_000, 200_000_000, 5_000));
        let found = check(&limits(), &slow, Mode::Enforce, cpu_judged(9.0, 8));
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("peak memory"));
    }

    #[test]
    fn the_median_of_the_runs_is_judged_and_a_missing_limit_fails() {
        let runs = [
            row("scan", 100_000, 100_000_000, 1_000),
            row("scan", 100_000, 900_000_000, 9_000),
            row("scan", 100_000, 101_000_000, 1_010),
        ];
        assert!(check(&limits(), &runs, Mode::Enforce, true).is_empty());
        let unknown = [row("disk", 100_000, 1, 1)];
        assert!(check(&limits(), &unknown, Mode::Enforce, true)[0].contains("no limit"));
    }
}
