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
    /// Remove a folder's protection: its .ddc-keep file, never the folder.
    Keep {
        #[command(subcommand)]
        action: KeepAction,
    },
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
            Command::Keep { action: KeepAction::Remove(a) } => a.json,
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
    /// Read no modification times inside findings, so a load test can measure
    /// what they cost (story "Review window: sort a group's rows by name, size
    /// or last change"). A user-data folder's own entries keep theirs.
    #[arg(long, hide = true, conflicts_with = "background")]
    pub no_times: bool,
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
pub enum KeepAction {
    /// Remove the .ddc-keep file in one folder; the next scan lists the folder in its own group.
    Remove(KeepRemoveArgs),
}

#[derive(Debug, Args)]
pub struct KeepRemoveArgs {
    /// The folder that holds the .ddc-keep file, as an absolute path (the
    /// scan report's `protection.markers[].folder`).
    #[arg(value_name = "FOLDER")]
    pub folder: PathBuf,
    /// The folder's device as the scan report recorded it. With
    /// --folder-ino, nothing is asked; without them, a terminal asks.
    #[arg(long, value_name = "DEV", requires = "folder_ino")]
    pub folder_dev: Option<u64>,
    /// The folder's inode as the scan report recorded it.
    #[arg(long, value_name = "INO", requires = "folder_dev")]
    pub folder_ino: Option<u64>,
    /// Print the report as JSON (schema: docs/keep.schema.json).
    #[arg(long)]
    pub json: bool,
    /// Where the last scan, the lock and the journal live (default: ~/Library/Application Support/DeepDriveCleaner).
    #[arg(long, value_name = "DIR")]
    pub data_dir: Option<PathBuf>,
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
    /// Look for Claude Code's temporary folder under this folder instead of
    /// `/` (tests).
    #[arg(long, hide = true, value_name = "DIR")]
    pub system_root: Option<PathBuf>,
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

/// Every subcommand and argument, hidden ones included, is called by a case
/// of `tests/cases.json` or listed in its `uncovered` with a reason (story
/// "CLI invocation cases: a table of ddc calls and the suggested commands
/// they print", DDC-005). `--help` hides some, so the rule reads clap's tree.
#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use clap::parser::ValueSource;
    use clap::{Arg, ArgAction, Command as Clap, CommandFactory as _};
    use serde_json::Value;

    use super::Cli;

    const CASES: &str = include_str!("../tests/cases.json");

    /// Every name of `cmd`'s tree: a subcommand by its path (`disk agents`),
    /// an argument by its path and long name (`scan --background`), or
    /// `<id>` when it has none (`scan <paths>`). Not clap's own `help` and
    /// `version`.
    fn names(cmd: &Clap, path: &str, out: &mut BTreeSet<String>) {
        for a in cmd.get_arguments().filter(|a| !matches!(a.get_id().as_str(), "help" | "version")) {
            out.insert(format!("{path} {}", arg_name(a)).trim().to_owned());
        }
        for sub in cmd.get_subcommands().filter(|s| s.get_name() != "help") {
            let p = format!("{path} {}", sub.get_name()).trim().to_owned();
            out.insert(p.clone());
            names(sub, &p, out);
        }
    }

    fn arg_name(a: &Arg) -> String {
        a.get_long().map_or_else(|| format!("<{}>", a.get_id()), |l| format!("--{l}"))
    }

    /// The names one call covers: each subcommand on its path, and each
    /// argument given on the command line. A call clap refuses covers nothing.
    fn covered(cmd: &Clap, args: &[String], out: &mut BTreeSet<String>) {
        let Ok(mut m) = cmd
            .clone()
            .try_get_matches_from(std::iter::once("deepdrivecleaner".to_owned()).chain(args.iter().cloned()))
        else {
            return;
        };
        let mut cmd = cmd.clone();
        let mut path = String::new();
        loop {
            for a in cmd.get_arguments() {
                if m.value_source(a.get_id().as_str()) == Some(ValueSource::CommandLine) {
                    out.insert(format!("{path} {}", arg_name(a)).trim().to_owned());
                }
            }
            let Some((name, sub_m)) = m.remove_subcommand() else { break };
            path = format!("{path} {name}").trim().to_owned();
            out.insert(path.clone());
            cmd = cmd.find_subcommand(&name).unwrap().clone();
            m = sub_m;
        }
    }

    /// The names of `cmd` neither `calls` nor `uncovered` cover.
    fn missing(cmd: &Clap, calls: &[Vec<String>], uncovered: &BTreeSet<String>) -> Vec<String> {
        let mut all = BTreeSet::new();
        names(cmd, "", &mut all);
        let mut seen = BTreeSet::new();
        for call in calls {
            covered(cmd, call, &mut seen);
        }
        all.into_iter().filter(|n| !seen.contains(n) && !uncovered.contains(n)).collect()
    }

    struct Cases {
        calls: Vec<Vec<String>>,
        uncovered: BTreeSet<String>,
        data_dir_commands: BTreeSet<String>,
    }

    /// The calls as the runner makes them: placeholders become `1` (a number
    /// and a path), and `--data-dir` is added where the command has it.
    fn cases() -> Cases {
        let v: Value = serde_json::from_str(CASES).unwrap();
        let strings = |v: &Value| -> Vec<String> {
            v.as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_owned()).collect()
        };
        let data_dir_commands: BTreeSet<String> = strings(&v["data_dir_commands"]).into_iter().collect();
        let calls = v["cases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                let mut args: Vec<String> =
                    strings(&c["args"]).into_iter().map(|a| if a.contains('{') { "1".to_owned() } else { a }).collect();
                let two = args.get(..2).map(|w| w.join(" ")).unwrap_or_default();
                if data_dir_commands.contains(&two) || args.first().is_some_and(|a| data_dir_commands.contains(a)) {
                    args.extend(["--data-dir".to_owned(), "1".to_owned()]);
                }
                args
            })
            .collect();
        let uncovered =
            v["uncovered"].as_array().unwrap().iter().map(|u| u["name"].as_str().unwrap().to_owned()).collect();
        Cases { calls, uncovered, data_dir_commands }
    }

    #[test]
    fn every_command_and_argument_is_in_a_case_or_uncovered() {
        let c = cases();
        let missing = missing(&Cli::command(), &c.calls, &c.uncovered);
        assert!(missing.is_empty(), "in no case of tests/cases.json and not in its uncovered: {missing:#?}");
    }

    #[test]
    fn every_uncovered_name_is_a_real_command_or_argument() {
        let mut all = BTreeSet::new();
        names(&Cli::command(), "", &mut all);
        let stale: Vec<String> = cases().uncovered.into_iter().filter(|u| !all.contains(u)).collect();
        assert!(stale.is_empty(), "uncovered names nothing the CLI has: {stale:?}");
    }

    #[test]
    fn data_dir_commands_are_the_commands_with_data_dir() {
        fn with_data_dir(cmd: &Clap, path: &str, out: &mut BTreeSet<String>) {
            if cmd.get_arguments().any(|a| a.get_id() == "data_dir") {
                out.insert(path.to_owned());
            }
            for sub in cmd.get_subcommands() {
                with_data_dir(sub, format!("{path} {}", sub.get_name()).trim(), out);
            }
        }
        let mut want = BTreeSet::new();
        with_data_dir(&Cli::command(), "", &mut want);
        assert_eq!(cases().data_dir_commands, want);
    }

    #[test]
    fn the_rule_names_what_no_call_covers() {
        let cmd = Clap::new("x")
            .subcommand(Clap::new("a").arg(Arg::new("h").long("h").hide(true).action(ArgAction::SetTrue)));
        let none = BTreeSet::new();
        assert_eq!(missing(&cmd, &[], &none), ["a", "a --h"]);
        assert_eq!(missing(&cmd, &[vec!["a".into()]], &none), ["a --h"]);
        assert!(missing(&cmd, &[vec!["a".into(), "--h".into()]], &none).is_empty());
        assert!(missing(&cmd, &[], &["a".to_owned(), "a --h".to_owned()].into()).is_empty());
        assert_eq!(missing(&cmd, &[vec!["a".into(), "--nope".into()]], &none), ["a", "a --h"], "a refused call");
    }
}
