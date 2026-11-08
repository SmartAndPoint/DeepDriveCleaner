//! `ddc scan`: read-only report of what can be freed.

use std::io::{IsTerminal as _, Write as _};
use std::process::ExitCode;

use anyhow::Context as _;
use ddc_core::env::Env;
use ddc_core::model::WrittenBy;
use ddc_core::progress::WalkStep;
use ddc_core::report::volumes::SystemStatFs;
use ddc_core::report::{self, ScanOptions};
use ddc_core::scan::{Progress, WalkOptions};
use ddc_core::settings::{self, ScanFlags};
use ddc_core::store::{self, Kept, LockMode};

use crate::cli::ScanArgs;
use crate::{progress, prompt, render};

pub fn run(args: ScanArgs) -> anyhow::Result<ExitCode> {
    if args.background {
        return super::background::run(&args, &crate::platform::SystemNotifier, &SystemStatFs);
    }
    let env = Env::from_process(args.data_dir.clone())?;
    let settings = settings::load(&env)?;
    let given_roots = !args.paths.is_empty();
    let resolved = settings.resolve(
        &env,
        &ScanFlags {
            paths: args.paths,
            excludes: args.exclude,
            skips: args.skip,
            min_age_days: args.min_age,
            worktree_min_age_days: args.worktree_min_age,
        },
    )?;
    super::note_missing(&resolved.missing);
    // A scan changes nothing: a dropped protection is only worth a warning.
    if !resolved.dropped_excludes.is_empty() {
        eprintln!("ddc: {}", prompt::dropped_excludes(&resolved.dropped_excludes, env.home()));
    }
    report::require_own_roots(&env, &resolved.options)?;
    let _lock = store::try_lock(&env, LockMode::Shared, ddc_core::holders::HolderCommand::Scan)?;
    let walk = WalkOptions { threads: args.threads, claim_times: !args.no_times, ..WalkOptions::default() };
    let opts = ScanOptions { walk, ..resolved.options };
    let reporter = progress::Reporter::new(args.progress.is_some());
    let progress = Progress::default();
    let mut report =
        reporter.walk(&progress, "scanning", WalkStep::Scan, None, || report::scan(&env, &opts, &progress))?;
    reporter.walk_done(WalkStep::Scan, &report.totals, None);
    let defaults = super::default_roots(&env, &settings);
    let saved = store::write_last_scan(&env, &mut report, WrittenBy::Scan, defaults.as_deref())
        .context("cannot save the scan report")?;
    if let Some(warning) = super::not_kept(&env, &saved.kept) {
        eprintln!("ddc: warning: {warning}");
    }
    let own_roots = super::own_roots(given_roots, &report, &env, &settings);
    if own_roots && env.data_dir_is_default() {
        eprintln!("ddc: The app will show this report until its next scan.");
    }
    for root in report.scan_roots.iter().filter(|r| !report.volumes.iter().any(|v| v.roots.contains(r))) {
        eprintln!("ddc: no volume information for {}; its free space is unknown", root.display());
    }

    let mut stdout = std::io::stdout().lock();
    let written = if args.json {
        serde_json::to_writer_pretty(&mut stdout, &report).map_err(std::io::Error::from).and_then(|()| writeln!(stdout))
    } else {
        let color = render::use_color(std::io::stdout().is_terminal(), Env::no_color());
        render::report(&mut stdout, &report, &render::Style { color, all: args.all }, own_roots).and_then(|()| {
            // What grew since the report this save kept (story "ddc changes:
            // what grew between the last two scans").
            match saved.kept {
                Kept::Kept { .. } => super::changes::after_scan(&mut stdout, &env, report),
                Kept::Nothing | Kept::Failed(_) => Ok(()),
            }
        })
    };
    match written {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(ExitCode::SUCCESS),
        other => other.map(|()| ExitCode::SUCCESS).map_err(Into::into),
    }
}
