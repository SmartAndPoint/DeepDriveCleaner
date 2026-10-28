//! The load results as TSV, in a fixed column order: a `#` line with the
//! run's context, a header row, one row per command and run. Story
//! "Complexity guard: operation counts and scaling exponents" reuses it.

use std::fmt::Write as _;

pub const COLUMNS: [&str; 13] = [
    "profile",
    "size",
    "seed",
    "entries",
    "command",
    "run",
    "wall_ms",
    "user_ms",
    "sys_ms",
    "cpu_ms",
    "max_rss_bytes",
    "threads_max",
    "exit",
];

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Row {
    pub profile: String,
    pub size: String,
    pub seed: u64,
    pub entries: u64,
    pub command: String,
    /// From 1.
    pub run: u32,
    pub wall_ms: u128,
    pub user_ms: u128,
    pub sys_ms: u128,
    pub cpu_ms: u128,
    pub max_rss_bytes: u64,
    pub threads_max: u32,
    /// The exit code, or `sig<N>` for a child a signal killed.
    pub exit: String,
}

impl Row {
    pub fn line(&self) -> String {
        [
            self.profile.clone(),
            self.size.clone(),
            self.seed.to_string(),
            self.entries.to_string(),
            self.command.clone(),
            self.run.to_string(),
            self.wall_ms.to_string(),
            self.user_ms.to_string(),
            self.sys_ms.to_string(),
            self.cpu_ms.to_string(),
            self.max_rss_bytes.to_string(),
            self.threads_max.to_string(),
            self.exit.clone(),
        ]
        .join("\t")
    }
}

/// What the `#` line records.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Meta {
    pub date: String,
    pub commit: String,
    pub model: String,
    pub ncpu: usize,
    /// The 1-minute load average at the start and the end.
    pub load_start: f64,
    pub load_end: f64,
    /// `record` or `enforce`.
    pub mode: String,
    pub seed: u64,
}

impl Meta {
    pub fn line(&self) -> String {
        format!(
            "# date={} commit={} model={} ncpu={} load_start={:.2} load_end={:.2} mode={} seed={}",
            self.date, self.commit, self.model, self.ncpu, self.load_start, self.load_end, self.mode, self.seed
        )
    }

    /// Reads a `#` line written by [`Meta::line`].
    pub fn parse(line: &str) -> Option<Self> {
        let fields: std::collections::HashMap<&str, &str> =
            line.strip_prefix("# ")?.split(' ').filter_map(|kv| kv.split_once('=')).collect();
        Some(Self {
            date: fields.get("date")?.to_string(),
            commit: fields.get("commit")?.to_string(),
            model: fields.get("model")?.to_string(),
            ncpu: fields.get("ncpu")?.parse().ok()?,
            load_start: fields.get("load_start")?.parse().ok()?,
            load_end: fields.get("load_end")?.parse().ok()?,
            mode: fields.get("mode")?.to_string(),
            seed: fields.get("seed")?.parse().ok()?,
        })
    }
}

/// A whole table: the `#` line, the header and the rows.
pub fn render(meta: &Meta, rows: &[String]) -> String {
    let mut out = format!("{}\n{}\n", meta.line(), COLUMNS.join("\t"));
    for row in rows {
        let _ = writeln!(out, "{row}");
    }
    out
}

/// Joins the tables of one run's profiles, given in run order, under one
/// `#` line: the first table's context and start load, the last end load.
pub fn merge(tables: &[String]) -> anyhow::Result<String> {
    let mut meta: Option<Meta> = None;
    let mut rows = Vec::new();
    for (i, t) in tables.iter().enumerate() {
        let mut lines = t.lines();
        let m = lines.next().and_then(Meta::parse).ok_or_else(|| anyhow::anyhow!("table {i}: no # line"))?;
        if lines.next() != Some(COLUMNS.join("\t").as_str()) {
            anyhow::bail!("table {i}: the header is not {COLUMNS:?}");
        }
        rows.extend(lines.filter(|l| !l.is_empty()).map(str::to_owned));
        meta = Some(match meta {
            None => m,
            Some(first) => Meta { load_end: m.load_end, ..first },
        });
    }
    let meta = meta.ok_or_else(|| anyhow::anyhow!("no table to merge"))?;
    Ok(render(&meta, &rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(load_start: f64, load_end: f64) -> Meta {
        Meta {
            date: "2026-10-29".into(),
            commit: "abc1234".into(),
            model: "Mac16,8".into(),
            ncpu: 12,
            load_start,
            load_end,
            mode: "record".into(),
            seed: 1,
        }
    }

    #[test]
    fn the_meta_line_reads_back() {
        let m = meta(1.5, 2.25);
        assert_eq!(Meta::parse(&m.line()), Some(m));
    }

    #[test]
    fn merged_tables_keep_every_row_under_one_line() {
        let a = render(&meta(1.0, 2.0), &["a\t1".into()]);
        let b = render(&meta(3.0, 4.0), &["b\t1".into(), "b\t2".into()]);
        let merged = merge(&[a, b]).unwrap();
        let lines: Vec<&str> = merged.lines().collect();
        assert_eq!(Meta::parse(lines[0]), Some(meta(1.0, 4.0)));
        assert_eq!(lines[1], COLUMNS.join("\t"));
        assert_eq!(&lines[2..], ["a\t1", "b\t1", "b\t2"]);
        assert!(merge(&["no meta\n".into()]).is_err());
    }
}
