//! Command-line definition.

use std::path::PathBuf;
use std::time::UNIX_EPOCH;

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
    /// List the rules by group: what each type is and whether settings.json can switch it off.
    Rules(RulesArgs),
    /// Show what uses the whole startup disk, outside your home folder too (read-only).
    Disk(DiskArgs),
    /// Scan in the background on a schedule and notify; never deletes anything.
    Schedule {
        #[command(subcommand)]
        action: ScheduleAction,
    },
    /// Record what the Mac app did with a notification (the app's own calls).
    #[command(hide = true)]
    Notify {
        #[command(subcommand)]
        action: NotifyAction,
    },
    /// Measure allocated size like `du -skx` (diagnostics and benchmarks).
    #[command(hide = true)]
    Du(DuArgs),
    /// Print process diagnostics (dataless policy, open-file limit, volume, process snapshot).
    #[command(hide = true)]
    Diag,
}

impl Command {
    /// The command was asked for JSON on stdout, where a refusal's envelope
    /// goes in its place (ADR "Platform seam: one platform module per OS
    /// with declared capabilities", §7). `--progress json` alone is not:
    /// its events go to stderr.
    pub fn prints_json(&self) -> bool {
        match self {
            Command::Scan(args) => args.json || args.notify == Some(NotifyFormat::Json),
            Command::Clean(args) => args.json,
            Command::Rules(args) => args.json,
            Command::Disk(args) => match &args.action {
                None => args.json,
                Some(DiskAction::Agents(a)) => a.json,
                Some(DiskAction::Free(a)) => a.json,
                Some(DiskAction::BrewPackages(a)) => a.json,
            },
            Command::Schedule { .. } | Command::Notify { .. } | Command::Du(_) | Command::Diag => false,
        }
    }
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
    /// Never read this folder: not scanned and never cleaned. Adds to
    /// settings.json's skips (repeatable).
    #[arg(long, value_name = "PATH")]
    pub skip: Vec<PathBuf>,
    /// Projects untouched for at least this many days are selected for cleanup
    /// (default: settings.json, else 14).
    #[arg(long, value_name = "DAYS", value_parser = clap::value_parser!(u64).range(0..=3650))]
    pub min_age: Option<u64>,
    /// Where the last scan and the lock live (default: ~/Library/Application Support/DeepDriveCleaner).
    #[arg(long, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,
    /// Worker threads (default: available parallelism).
    #[arg(long)]
    pub threads: Option<usize>,
    /// Write progress events to stderr, one JSON line each (docs/progress.schema.json).
    #[arg(long, value_enum, value_name = "FORMAT", conflicts_with = "background")]
    pub progress: Option<ProgressFormat>,
    /// Run as the scheduled background agent: no output, skip TCC-protected folders, notify.
    #[arg(long, hide = true)]
    pub background: bool,
    /// Background mode: do not show a notification.
    #[arg(long, hide = true)]
    pub no_notify: bool,
    /// Background mode: print the notify decision (docs/notify.schema.json)
    /// instead of notifying; the app shows it and records it with `notify ack`.
    #[arg(long, hide = true, value_enum, value_name = "FORMAT", requires = "background", conflicts_with = "no_notify")]
    pub notify: Option<NotifyFormat>,
    /// Background mode: notify when at least this much can be freed (default:
    /// settings.json, else 10G).
    #[arg(long, hide = true, value_name = "SIZE", value_parser = parse_size)]
    pub threshold: Option<u64>,
}

#[derive(Debug, Args)]
pub struct RulesArgs {
    /// Print the rules as JSON (schema: docs/rules.schema.json).
    #[arg(long)]
    pub json: bool,
    /// Print another platform's rules, for review (default: this one's).
    #[arg(long, value_enum, value_name = "OS")]
    pub platform: Option<PlatformArg>,
}

/// A platform `ddc rules --platform` can print.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PlatformArg {
    Macos,
    Linux,
    Windows,
}

impl From<PlatformArg> for ddc_core::platform::Platform {
    fn from(p: PlatformArg) -> Self {
        match p {
            PlatformArg::Macos => Self::Macos,
            PlatformArg::Linux => Self::Linux,
            PlatformArg::Windows => Self::Windows,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum NotifyAction {
    /// A notification for this many reclaimable bytes was shown now.
    Ack {
        #[arg(long, value_name = "BYTES")]
        bytes: u64,
        #[arg(long, value_name = "DIR")]
        data_dir: Option<PathBuf>,
    },
    /// No notification from scans that start before this time ("Remind me tomorrow").
    Snooze {
        /// RFC 3339 in UTC, such as 2026-10-20T09:00:00Z.
        #[arg(long, value_name = "TIME", value_parser = parse_utc)]
        until: u64,
        #[arg(long, value_name = "DIR")]
        data_dir: Option<PathBuf>,
    },
}

/// RFC 3339 in UTC (`Z`), as Unix seconds.
fn parse_utc(s: &str) -> Result<u64, String> {
    let at = humantime::parse_rfc3339(s).map_err(|e| format!("{e} (expected UTC such as 2026-10-20T09:00:00Z)"))?;
    at.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).map_err(|e| e.to_string())
}

#[derive(Debug, Subcommand)]
pub enum ScheduleAction {
    /// Install (or update) the LaunchAgent that scans on a schedule.
    Install {
        #[arg(long, value_enum, default_value_t = IntervalArg::Daily)]
        interval: IntervalArg,
        /// Notify when at least this much can be freed, or free space is below 10%
        /// (default: settings.json, else 10G).
        #[arg(long, value_name = "SIZE", value_parser = parse_size)]
        threshold: Option<u64>,
    },
    /// Remove the LaunchAgent.
    Uninstall,
    /// Show whether the agent is installed, loaded and current.
    Status,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum NotifyFormat {
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProgressFormat {
    Json,
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
    /// Projects untouched for at least this many days are selected (default:
    /// settings.json, else 14).
    #[arg(long, value_name = "DAYS", value_parser = clap::value_parser!(u64).range(0..=3650))]
    pub min_age: Option<u64>,
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
    /// Never read this folder: not scanned and never cleaned. Adds to
    /// settings.json's skips (repeatable).
    #[arg(long, value_name = "PATH")]
    pub skip: Vec<PathBuf>,
    /// Where the last scan, the lock and the logs live.
    #[arg(long, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,
    /// Clean exactly the items in this JSON list (`-` reads stdin): a selection
    /// confirmed elsewhere, such as the Mac app's sheet. Rescans and re-checks
    /// every item; with --dry-run, only predicts each one without a rescan.
    #[arg(long, value_name = "FILE", conflicts_with_all = ["permanent", "yes", "classes", "categories", "min_size"])]
    pub items: Option<PathBuf>,
    /// With --items: the report as JSON (with --dry-run, one JSON line per item).
    #[arg(long, requires = "items")]
    pub json: bool,
    /// With --items: report this many local Time Machine snapshots instead of
    /// asking `tmutil`, so a load test runs no tool (story "Memory and CPU
    /// under load").
    #[arg(long, hide = true, value_name = "N", requires = "items")]
    pub local_snapshots: Option<usize>,
    /// Write progress events to stderr, one JSON line each (docs/progress.schema.json).
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub progress: Option<ProgressFormat>,
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
#[command(args_conflicts_with_subcommands = true)]
pub struct DiskArgs {
    #[command(subcommand)]
    pub action: Option<DiskAction>,
    /// Print the report as JSON (schema: docs/disk.schema.json).
    #[arg(long)]
    pub json: bool,
    /// Where the last scan and the last disk report live (default: ~/Library/Application Support/DeepDriveCleaner).
    #[arg(long, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,
    /// Worker threads (default: available parallelism).
    #[arg(long)]
    pub threads: Option<usize>,
    /// Write progress events to stderr, one JSON line each (docs/progress.schema.json).
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub progress: Option<ProgressFormat>,
    /// Look for the system's folders and tools under this folder instead of
    /// `/` (tests).
    #[arg(long, hide = true, value_name = "DIR")]
    pub system_root: Option<PathBuf>,
    /// Entries read in each temporary folder before the rest is left unread (tests).
    #[arg(long, hide = true, value_name = "N")]
    pub temp_entry_budget: Option<u64>,
}

#[derive(Debug, Subcommand)]
pub enum DiskAction {
    /// What coding-agent harnesses keep under your home folder, and their live sessions (read-only).
    Agents(AgentsArgs),
    /// Free an area with its own tool: delete a simulator runtime, or run `brew cleanup`.
    Free(FreeArgs),
    /// The Homebrew formulae you installed, their sizes and what they need (read-only).
    BrewPackages(BrewPackagesArgs),
}

#[derive(Debug, Args)]
pub struct BrewPackagesArgs {
    /// Print the listing as JSON (schema: docs/brew-packages.schema.json).
    #[arg(long)]
    pub json: bool,
    /// Where settings.json lives (default: ~/Library/Application Support/DeepDriveCleaner).
    #[arg(long, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,
    /// Look for the system's folders and tools under this folder instead of
    /// `/` (tests).
    #[arg(long, hide = true, value_name = "DIR")]
    pub system_root: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct FreeArgs {
    /// Run exactly the actions in this list ("-" for stdin): the confirmation
    /// of the app's sheet (schema: docs/disk-free.schema.json). Without it, a
    /// terminal asks for each action.
    #[arg(long, value_name = "FILE|-")]
    pub actions: Option<PathBuf>,
    /// Run each tool's own dry run instead: what it would remove, and how much.
    #[arg(long)]
    pub dry_run: bool,
    /// Print the report as JSON (schema: docs/disk-free.schema.json).
    #[arg(long)]
    pub json: bool,
    /// Where the last scan, the last disk report and the journal live (default: ~/Library/Application Support/DeepDriveCleaner).
    #[arg(long, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,
    /// Look for the system's folders and tools under this folder instead of
    /// `/` (tests).
    #[arg(long, hide = true, value_name = "DIR")]
    pub system_root: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct AgentsArgs {
    /// Print the report as JSON (schema: docs/agents.schema.json).
    #[arg(long)]
    pub json: bool,
    /// Do not measure the folders: only the sessions, in a fraction of a second.
    #[arg(long)]
    pub no_sizes: bool,
    /// Where settings.json lives (default: ~/Library/Application Support/DeepDriveCleaner).
    #[arg(long, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,
    /// Worker threads (default: available parallelism).
    #[arg(long)]
    pub threads: Option<usize>,
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
