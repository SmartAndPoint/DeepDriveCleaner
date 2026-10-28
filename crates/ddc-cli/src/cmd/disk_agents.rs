//! `ddc disk agents`: what coding-agent harnesses keep under the home
//! folder, and their live sessions (story "Coding agents in the Whole disk
//! view"). Read-only and lock-free: it saves nothing and sends no
//! notification.

use std::io::{IsTerminal as _, Write as _};
use std::process::ExitCode;

use anyhow::Context as _;
use ddc_core::agents::{self, AgentsOptions};
use ddc_core::env::Env;
use ddc_core::settings::ScanFlags;
use ddc_core::sys::{procs, signals};

use crate::cli::AgentsArgs;
use crate::render;

pub fn run(args: &AgentsArgs) -> anyhow::Result<ExitCode> {
    let cancel = signals::install().context("cannot install the signal handler")?;
    let env = Env::from_process(args.data_dir.clone())?;
    // The skips only: they bind the record read as well as the walk.
    let resolved = super::resolve(&env, ScanFlags { paths: vec![env.home().to_path_buf()], ..ScanFlags::default() })?;
    super::note_missing(&resolved.missing);
    let opts = AgentsOptions { sizes: !args.no_sizes, threads: args.threads, user_skips: resolved.options.user_skips };
    let report = agents::report(&env, &opts, &procs::current_user_processes_unopened(), cancel);
    if let Some(sig) = signals::received() {
        signals::reraise(sig);
    }

    let mut stdout = std::io::stdout().lock();
    let written = if args.json {
        serde_json::to_writer_pretty(&mut stdout, &report).map_err(std::io::Error::from).and_then(|()| writeln!(stdout))
    } else {
        let color = render::use_color(std::io::stdout().is_terminal(), Env::no_color());
        render::agents(&mut stdout, &report, env.home(), &render::Style { color, all: false })
    };
    match written {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(ExitCode::SUCCESS),
        other => other.map(|()| ExitCode::SUCCESS).map_err(Into::into),
    }
}
