//! `ddc scan`: read-only report of what can be freed.

use std::io::{IsTerminal as _, Write as _};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Context as _;
use ddc_core::env::Env;
use ddc_core::report::{self, ScanOptions};
use ddc_core::scan::{Progress, WalkOptions};
use ddc_core::store::{self, LockMode};

use crate::cli::ScanArgs;
use crate::{progress, render};

pub fn run(args: ScanArgs) -> anyhow::Result<ExitCode> {
    if args.background {
        return super::background::run(&args);
    }
    let env = Env::from_process(args.data_dir.clone())?;
    let _lock = store::try_lock(&env, LockMode::Shared)?;
    let opts = ScanOptions {
        roots: args.paths,
        excludes: args.exclude,
        min_age: Duration::from_secs(args.min_age * 86_400),
        walk: WalkOptions { threads: args.threads, ..WalkOptions::default() },
        ..ScanOptions::default()
    };
    let progress = Progress::default();
    let report = progress::with_spinner(&progress, "scanning", || report::scan(&env, &opts, &progress))?;
    store::write_last_scan(&env, &report).context("cannot save the scan report")?;

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
