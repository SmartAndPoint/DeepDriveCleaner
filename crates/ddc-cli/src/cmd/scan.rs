//! `ddc scan`: read-only report of what can be freed.

use std::io::{IsTerminal as _, Write as _};
use std::process::ExitCode;

use anyhow::Context as _;
use ddc_core::env::Env;
use ddc_core::progress::WalkStep;
use ddc_core::report::volumes::SystemStatFs;
use ddc_core::report::{self, ScanOptions};
use ddc_core::scan::{Progress, WalkOptions};
use ddc_core::settings::ScanFlags;
use ddc_core::store::{self, LockMode};

use crate::cli::ScanArgs;
use crate::{notify, progress, prompt, render};

pub fn run(args: ScanArgs) -> anyhow::Result<ExitCode> {
    if args.background {
        return super::background::run(&args, &notify::Osascript, &SystemStatFs);
    }
    let env = Env::from_process(args.data_dir.clone())?;
    let resolved = super::resolve(
        &env,
        ScanFlags { paths: args.paths, excludes: args.exclude, skips: args.skip, min_age_days: args.min_age },
    )?;
    super::note_missing(&resolved.missing);
    // A scan changes nothing: a dropped protection is only worth a warning.
    if !resolved.dropped_excludes.is_empty() {
        eprintln!("ddc: {}", prompt::dropped_excludes(&resolved.dropped_excludes, env.home()));
    }
    let _lock = store::try_lock(&env, LockMode::Shared)?;
    let opts =
        ScanOptions { walk: WalkOptions { threads: args.threads, ..WalkOptions::default() }, ..resolved.options };
    let reporter = progress::Reporter::new(args.progress.is_some());
    let progress = Progress::default();
    let report = reporter.walk(&progress, "scanning", WalkStep::Scan, None, || report::scan(&env, &opts, &progress))?;
    reporter.walk_done(WalkStep::Scan, &report.totals, None);
    store::write_last_scan(&env, &report).context("cannot save the scan report")?;
    for root in report.scan_roots.iter().filter(|r| !report.volumes.iter().any(|v| v.roots.contains(r))) {
        eprintln!("ddc: no volume information for {}; its free space is unknown", root.display());
    }

    let mut stdout = std::io::stdout().lock();
    let written = if args.json {
        serde_json::to_writer_pretty(&mut stdout, &report).map_err(std::io::Error::from).and_then(|()| writeln!(stdout))
    } else {
        let color = render::use_color(std::io::stdout().is_terminal(), Env::no_color());
        render::report(&mut stdout, &report, &render::Style { color, all: args.all })
    };
    match written {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(ExitCode::SUCCESS),
        other => other.map(|()| ExitCode::SUCCESS).map_err(Into::into),
    }
}
