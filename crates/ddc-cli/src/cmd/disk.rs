//! `ddc disk`: what uses the whole startup disk, outside the home folder too
//! (story "Whole disk: what lies outside the home folder"). Read-only and
//! lock-free: it removes nothing and only reads `last-scan.json`.

use std::io::{IsTerminal as _, Write as _};
use std::process::ExitCode;

use anyhow::Context as _;
use ddc_core::disk::{self, DiskOptions, DiskPorts, Sources};
use ddc_core::env::Env;
use ddc_core::exec::system::SystemCommands;
use ddc_core::progress::{WalkEvent, WalkStep};
use ddc_core::report::volumes::SystemStatFs;
use ddc_core::scan::Progress;
use ddc_core::settings::ScanFlags;
use ddc_core::store;
use ddc_core::sys::signals;

use crate::cli::DiskArgs;
use crate::{progress, render};

pub fn run(args: DiskArgs) -> anyhow::Result<ExitCode> {
    // First: a SIGTERM from here on still writes the report.
    let cancel = signals::install().context("cannot install the signal handler")?;
    let env = Env::from_process(args.data_dir.clone())?;
    // The skips only: the scan roots do not matter to the whole disk.
    let resolved = super::resolve(&env, ScanFlags { paths: vec![env.home().to_path_buf()], ..ScanFlags::default() })?;
    super::note_missing(&resolved.missing);
    let sources = args.system_root.as_deref().map_or_else(Sources::macos, Sources::under);
    let mut opts =
        DiskOptions { threads: args.threads, user_skips: resolved.options.user_skips, ..DiskOptions::default() };
    if let Some(budget) = args.temp_entry_budget {
        opts.temp_entry_budget = budget;
    }
    let commands = SystemCommands::new(&env);
    let ports = DiskPorts { commands: &commands, statfs: &SystemStatFs };
    let reporter = progress::Reporter::new(args.progress.is_some());
    let walked = Progress::default();
    let report = reporter.walk(&walked, "measuring the disk", WalkStep::Disk, None, || {
        disk::measure(&env, &sources, &opts, &ports, &walked, cancel)
    });
    reporter.walk_done_counts(WalkEvent {
        step: WalkStep::Disk,
        files: report.walk.files,
        dirs: report.walk.dirs,
        bytes: report.walk.bytes,
        files_expected: None,
    });
    // Not `?`: a signal received still ends ddc by that signal.
    let saved = store::write_last_disk(&env, &report).context("cannot save the disk report");

    let mut stdout = std::io::stdout().lock();
    let written = if args.json {
        serde_json::to_writer_pretty(&mut stdout, &report).map_err(std::io::Error::from).and_then(|()| writeln!(stdout))
    } else {
        let color = render::use_color(std::io::stdout().is_terminal(), Env::no_color());
        render::disk(&mut stdout, &report, &render::Style { color, all: false })
    };
    drop(stdout);
    if let Some(sig) = signals::received() {
        signals::reraise(sig);
    }
    saved?;
    match written {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(ExitCode::SUCCESS),
        other => other.map(|()| ExitCode::SUCCESS).map_err(Into::into),
    }
}
