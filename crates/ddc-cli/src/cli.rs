//! Command-line definition.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "deepdrivecleaner",
    version,
    about = "DeepDriveCleaner: find and safely clean developer disk junk (short name: ddc)"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Find developer disk junk and show what can be freed (read-only).
    Scan(ScanArgs),
    /// Scan, show a cleanup plan and clean what you confirm.
    Clean(CleanArgs),
    /// Scan in the background on a schedule and notify; never deletes anything.
    Schedule {
        #[command(subcommand)]
        action: ScheduleAction,
    },
    /// Measure allocated size like `du -skx` (diagnostics and benchmarks).
    #[command(hide = true)]
    Du(DuArgs),
    /// Print process diagnostics (dataless policy, open-file limit, volume).
    #[command(hide = true)]
    Diag,
}

#[derive(Debug, Args)]
pub struct ScanArgs {
    /// Paths to scan (default: your home directory).
    pub paths: Vec<PathBuf>,
    /// Print the report as JSON (schema: docs/scan.schema.json).
    #[arg(long)]
    pub json: bool,
    /// Show every finding instead of the largest ones per group.
    #[arg(long)]
    pub all: bool,
    /// Never touch this path: it is reported as protected (repeatable).
    #[arg(long, value_name = "PATH")]
    pub exclude: Vec<PathBuf>,
    /// Projects untouched for at least this many days are selected for cleanup.
    #[arg(long, value_name = "DAYS", default_value_t = 14, value_parser = clap::value_parser!(u64).range(0..=3650))]
    pub min_age: u64,
    /// Where the last scan and the lock live (default: ~/Library/Application Support/DeepDriveCleaner).
    #[arg(long, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,
    /// Worker threads (default: available parallelism).
    #[arg(long)]
    pub threads: Option<usize>,
    /// Run as the scheduled background agent: no output, skip TCC-protected folders, notify.
    #[arg(long, hide = true)]
    pub background: bool,
    /// Background mode: do not show a notification.
    #[arg(long, hide = true)]
    pub no_notify: bool,
    /// Background mode: notify when at least this much can be freed.
    #[arg(long, hide = true, value_name = "SIZE", value_parser = parse_size, default_value = "10G")]
    pub threshold: u64,
}

#[derive(Debug, Subcommand)]
pub enum ScheduleAction {
    /// Install (or update) the LaunchAgent that scans on a schedule.
    Install {
        #[arg(long, value_enum, default_value_t = IntervalArg::Daily)]
        interval: IntervalArg,
        /// Notify when at least this much can be freed (or free space is below 10%).
        #[arg(long, value_name = "SIZE", value_parser = parse_size, default_value = "10G")]
        threshold: u64,
    },
    /// Remove the LaunchAgent.
    Uninstall,
    /// Show whether the agent is installed, loaded and current.
    Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum IntervalArg {
    Daily,
    Weekly,
}

#[derive(Debug, Args)]
pub struct CleanArgs {
    /// Paths to scan (default: your home directory).
    pub paths: Vec<PathBuf>,
    /// Only these classes (repeatable). With --yes the default is `safe`.
    #[arg(long = "class", value_enum, value_name = "CLASS")]
    pub classes: Vec<ClassArg>,
    /// Only these categories (repeatable).
    #[arg(long = "category", value_enum, value_name = "CATEGORY")]
    pub categories: Vec<CategoryArg>,
    /// Skip findings smaller than this (e.g. 100M, 2G).
    #[arg(long, value_name = "SIZE", value_parser = parse_size)]
    pub min_size: Option<u64>,
    /// Projects untouched for at least this many days are selected.
    #[arg(long, value_name = "DAYS", default_value_t = 14, value_parser = clap::value_parser!(u64).range(0..=3650))]
    pub min_age: u64,
    /// Show the plan and exit without changing anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Clean default-selected items of the given classes without asking (never `review`).
    #[arg(long)]
    pub yes: bool,
    /// Delete `review` items instead of moving them to the Trash.
    #[arg(long)]
    pub permanent: bool,
    /// Never touch this path (repeatable).
    #[arg(long, value_name = "PATH")]
    pub exclude: Vec<PathBuf>,
    /// Where the last scan, the lock and the logs live.
    #[arg(long, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ClassArg {
    Safe,
    Rebuildable,
    Review,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CategoryArg {
    ProjectArtifact,
    PackageCache,
    Xcode,
    VmContainer,
    Toolchain,
    MlModel,
    Ide,
    AppCache,
    GenericCache,
}

/// `100M`, `2G`, `512K`, `1024` (bytes).
fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, mult) = match s.char_indices().last() {
        Some((i, c)) if c.is_ascii_alphabetic() => {
            let mult = match c.to_ascii_uppercase() {
                'K' => 1u64 << 10,
                'M' => 1 << 20,
                'G' => 1 << 30,
                'T' => 1 << 40,
                _ => return Err(format!("unknown size unit in {s:?}")),
            };
            (&s[..i], mult)
        }
        _ => (s, 1),
    };
    let n: f64 = num.trim().parse().map_err(|_| format!("not a size: {s:?}"))?;
    Ok((n * mult as f64) as u64)
}

#[derive(Debug, Args)]
pub struct DuArgs {
    /// Paths to measure (default: the current directory).
    pub paths: Vec<PathBuf>,
    /// Directory reader implementation.
    #[arg(long, value_enum, default_value_t = ReaderArg::Bulk)]
    pub reader: ReaderArg,
    /// Worker threads (default: available parallelism).
    #[arg(long)]
    pub threads: Option<usize>,
    /// Also request the APFS private size of every file.
    #[arg(long)]
    pub private: bool,
    /// Apply the rule catalog and print findings as TSV:
    /// path, bytes, rule, class, private bytes, shared bytes, notes.
    #[arg(long)]
    pub rules: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ReaderArg {
    /// getattrlistbulk(2)
    Bulk,
    /// std::fs (readdir + lstat)
    Std,
}
