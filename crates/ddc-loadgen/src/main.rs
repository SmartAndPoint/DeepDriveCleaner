//! `ddc-loadgen`: builds a load tree and prints its manifest as JSON, or
//! runs the CLI on one and measures it (`run`).

use std::path::PathBuf;
use std::time::Instant;

use clap::{Parser, Subcommand};
use ddc_loadgen::desc::FsCaps;
use ddc_loadgen::limits::{self, Mode};
use ddc_loadgen::profiles::Profile;
use ddc_loadgen::run::RunArgs;
use ddc_loadgen::{generate, heap, leaks, manifest, run, table, target, verify};

#[derive(Parser)]
#[command(about = "Deterministic load trees for DeepDriveCleaner's load tests")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create a marked temporary folder and print its path.
    Mktemp,
    /// Remove every marked temporary folder of this user.
    Clean,
    /// Print every profile's name, one per line.
    Profiles,
    /// Build a tree and print its manifest.
    Build {
        profile: Profile,
        #[arg(long, default_value_t = 10_000)]
        entries: u64,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// A marked temporary folder (`mktemp`) or an attached image's mount point.
        #[arg(long)]
        into: PathBuf,
        /// The device `hdiutil attach` returned, for an image target.
        #[arg(long)]
        device: Option<String>,
        /// Scan the tree in process and fail on any difference from the manifest.
        #[arg(long)]
        verify: bool,
        /// Leave the manifest out of the output (its totals stay).
        #[arg(long)]
        brief: bool,
    },
    /// Build a tree, check it, then run the CLI on it and measure every run:
    /// `<size>-<profile>.tsv` in --out, a JSON report on stdout.
    Run {
        profile: Profile,
        #[arg(long, default_value_t = 10_000)]
        entries: u64,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// A marked temporary folder (`mktemp`) or an attached image's mount point.
        #[arg(long)]
        into: PathBuf,
        /// The device `hdiutil attach` returned, for an image target.
        #[arg(long)]
        device: Option<String>,
        /// The CLI to measure (the release build).
        #[arg(long)]
        cli: PathBuf,
        /// Runs of each command that reads the tree; the clean runs once.
        #[arg(long, default_value_t = 3)]
        repeat: u32,
        /// The size's name in the rows (default: S, M, L or XL from --entries).
        #[arg(long)]
        size: Option<String>,
        /// The date for the table's `#` line.
        #[arg(long, default_value = "-")]
        date: String,
        /// The commit for the table's `#` line.
        #[arg(long, default_value = "-")]
        commit: String,
        #[arg(long, default_value = "target/load")]
        out: PathBuf,
        /// `enforce` fails over a limit (CPU only on a quiet machine); `record` judges nothing.
        #[arg(long, value_enum, default_value_t = Mode::Record)]
        mode: Mode,
        #[arg(long, default_value = "bench/load-limits.tsv")]
        limits: PathBuf,
        /// The 1-minute load average when the whole run started (default: now).
        #[arg(long)]
        load_start: Option<f64>,
    },
    /// Build a tree and run one `scan --json` of it by a CLI built with
    /// `ddc-core/dhat-heap`; its heap profile goes to `--out`.
    Heap {
        profile: Profile,
        #[arg(long, default_value_t = 100_000)]
        entries: u64,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// A marked temporary folder (`mktemp`) or an attached image's mount point.
        #[arg(long)]
        into: PathBuf,
        /// The device `hdiutil attach` returned, for an image target.
        #[arg(long)]
        device: Option<String>,
        /// The CLI built with `ddc-core/dhat-heap`.
        #[arg(long)]
        cli: PathBuf,
        /// The size's name in the file name (default: S, M, L or XL from --entries).
        #[arg(long)]
        size: Option<String>,
        #[arg(long, default_value = "target/dhat")]
        out: PathBuf,
    },
    /// Print the limits from a small and a large run's tables (`make load-baseline`).
    Limits { small: PathBuf, large: PathBuf },
    /// `scan --json` of every profile's S tree under `leaks -atExit`: fails
    /// on any leak, or on a run that did not end normally.
    Leaks {
        /// The CLI to check (the cargo release build).
        #[arg(long)]
        cli: PathBuf,
        /// Where each profile's `leaks` output is kept.
        #[arg(long, default_value = "target/load")]
        out: PathBuf,
    },
    /// Join the tables of one run's profiles under one `#` line.
    Table {
        tables: Vec<PathBuf>,
        /// Write here instead of stdout; an existing file is never replaced.
        #[arg(long)]
        into: Option<PathBuf>,
    },
    /// Print a manifest without building (default APFS capabilities).
    Manifest {
        profile: Profile,
        #[arg(long, default_value_t = 10_000)]
        entries: u64,
        #[arg(long, default_value_t = 1)]
        seed: u64,
    },
}

fn main() -> anyhow::Result<()> {
    let out = match Cli::parse().cmd {
        Cmd::Mktemp => serde_json::json!({ "path": target::mktemp()? }),
        Cmd::Clean => serde_json::json!({ "removed": target::clean()? }),
        Cmd::Profiles => {
            Profile::ALL.iter().for_each(|p| println!("{}", p.name()));
            return Ok(());
        }
        Cmd::Manifest { profile, entries, seed } => {
            let caps = FsCaps { case_insensitive: true, normalization_insensitive: true, invalid_utf8: false };
            let m = manifest::manifest(profile.name(), seed, &profile.desc(entries, seed, caps));
            serde_json::json!({ "hash": m.hash(), "manifest": m })
        }
        Cmd::Run {
            profile,
            entries,
            seed,
            into,
            device,
            cli,
            repeat,
            size,
            date,
            commit,
            out,
            mode,
            limits,
            load_start,
        } => {
            let size = size.unwrap_or_else(|| size_name(entries));
            let a = RunArgs {
                profile,
                entries,
                seed,
                into,
                device,
                cli,
                repeat,
                size,
                date,
                commit,
                out,
                mode,
                limits,
                load_start,
            };
            let report = run::run(&a)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            // Advisory: the owner's app may scan on its own meanwhile.
            for change in &report.owner_changes {
                eprintln!("ddc-loadgen: {}: the owner's state changed during the run: {change}", profile.name());
            }
            if !report.passed() {
                anyhow::bail!(
                    "{}: {} differences from the manifest, {} failures",
                    profile.name(),
                    report.difference_count,
                    report.failure_count
                );
            }
            return Ok(());
        }
        Cmd::Leaks { cli, out } => {
            let checked = leaks::run(&cli, &out)?;
            println!("{}", serde_json::to_string_pretty(&checked)?);
            let failed: Vec<String> =
                checked.iter().filter(|c| !c.passed).map(|c| c.output.display().to_string()).collect();
            if !failed.is_empty() {
                anyhow::bail!("leaks: {} profile(s) failed; see {}", failed.len(), failed.join(", "));
            }
            return Ok(());
        }
        Cmd::Heap { profile, entries, seed, into, device, cli, size, out } => {
            let size = size.unwrap_or_else(|| size_name(entries));
            let a = heap::HeapArgs {
                profile,
                entries,
                seed,
                into: &into,
                device: device.as_deref(),
                cli: &cli,
                size: &size,
                out: &out,
            };
            serde_json::to_value(heap::run(&a)?)?
        }
        Cmd::Limits { small, large } => {
            let read = |p: &PathBuf| -> anyhow::Result<_> { table::parse(&std::fs::read_to_string(p)?) };
            let ((s, small), (l, large)) = (read(&small)?, read(&large)?);
            let note = format!(
                "base from {} entries, slope to {} entries, x{}; {} {} on {}, load {:.2} and {:.2} at the start",
                small.first().map_or(0, |r| r.entries),
                large.first().map_or(0, |r| r.entries),
                limits::HEADROOM,
                s.date,
                s.commit,
                s.model,
                s.load_start,
                l.load_start
            );
            print!("{}", limits::render(&limits::baseline(&small, &large)?, &note));
            return Ok(());
        }
        Cmd::Table { tables, into } => {
            let tables = tables.iter().map(std::fs::read_to_string).collect::<Result<Vec<_>, _>>()?;
            let merged = table::merge(&tables)?;
            match into {
                Some(path) => {
                    use std::io::Write as _;
                    let mut f = std::fs::File::create_new(&path)
                        .map_err(|e| anyhow::anyhow!("{}: {e}; an existing table is never replaced", path.display()))?;
                    f.write_all(merged.as_bytes())?;
                }
                None => print!("{merged}"),
            }
            return Ok(());
        }
        Cmd::Build { profile, entries, seed, into, device, verify: check, brief } => {
            let g = generate(profile, entries, seed, &into, device.as_deref())?;
            let started = Instant::now();
            let diffs = if check { verify::verify(&g.manifest, &g.layout, &g.home)? } else { Vec::new() };
            let verify_ms = check.then(|| started.elapsed().as_millis());
            let mut out = serde_json::json!({
                "profile": profile.name(),
                "entries": g.manifest.entries,
                "hash": g.manifest.hash(),
                "tree": g.layout.tree,
                "home": g.home,
                "data": g.layout.data,
                "build_ms": g.build_ms,
                "verify_ms": verify_ms,
                "differences": diffs,
                "difference_count": diffs.len(),
                "totals": g.manifest.totals,
                "manifest": g.manifest,
            });
            if brief {
                out["manifest"] = serde_json::Value::Null;
            }
            if !diffs.is_empty() {
                println!("{}", serde_json::to_string_pretty(&out)?);
                anyhow::bail!("{} differences from the manifest", diffs.len());
            }
            out
        }
    };
    println!("{}", serde_json::to_string_pretty(&out)?);
    Ok(())
}

/// The tier an entry count names in `scripts/load-image.sh`.
fn size_name(entries: u64) -> String {
    match entries {
        10_000 => "S".to_owned(),
        100_000 => "M".to_owned(),
        1_000_000 => "L".to_owned(),
        10_000_000 => "XL".to_owned(),
        n => n.to_string(),
    }
}
