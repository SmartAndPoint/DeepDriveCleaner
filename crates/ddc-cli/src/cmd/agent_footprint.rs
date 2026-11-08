//! `ddc agent footprint [ROOT]`: one project's checkouts, what each holds,
//! and what can go (story "ddc footprint: a scoped project report"; ADR
//! "Agent footprint", §4 and §7). It walks the main checkout and its
//! worktrees only, takes no lock, saves no report, sends no notification
//! and removes nothing.

use std::io::{IsTerminal as _, Write as _};
use std::process::ExitCode;

use anyhow::Context as _;
use ddc_core::env::Env;
use ddc_core::exec::{PROBE_TIMEOUT, SystemCommands};
use ddc_core::footprint::{self, Scope};
use ddc_core::scan::Progress;
use ddc_core::settings::ScanFlags;

use crate::cli::FootprintArgs;
use crate::render;

pub fn run(args: &FootprintArgs) -> anyhow::Result<ExitCode> {
    let env = Env::from_process(args.data_dir.clone())?;
    let root = match &args.root {
        Some(root) => root.clone(),
        None => std::env::current_dir().context("cannot read the current folder")?,
    };
    let commands = SystemCommands::new(&env);
    let scope = Scope::resolve(&root, env.home(), &commands, &env.tool_dirs(), PROBE_TIMEOUT)?;
    // Settings' skips, excludes and disabled rules apply as in a scan.
    let flags = ScanFlags {
        paths: vec![scope.main.clone()],
        min_age_days: args.min_age,
        worktree_min_age_days: args.worktree_min_age,
        ..ScanFlags::default()
    };
    let resolved = super::resolve(&env, flags)?;
    if !resolved.dropped_excludes.is_empty() {
        eprintln!("ddc: {}", crate::prompt::dropped_excludes(&resolved.dropped_excludes, env.home()));
    }
    super::note_missing(&resolved.missing);
    let opts = footprint::scan_options(&scope, resolved.options);
    let progress = Progress::default();
    let scoped = footprint::scoped(&env, scope, &opts, &progress, None)?;
    let report = footprint::report(&env, scoped, &opts);

    let mut stdout = std::io::stdout().lock();
    let written = if args.json {
        serde_json::to_writer_pretty(&mut stdout, &report).map_err(std::io::Error::from).and_then(|()| writeln!(stdout))
    } else {
        let color = render::use_color(std::io::stdout().is_terminal(), Env::no_color());
        render::footprint(&mut stdout, &report, env.home(), &render::Style { color, all: false })
    };
    match written {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(ExitCode::SUCCESS),
        other => other.map(|()| ExitCode::SUCCESS).map_err(Into::into),
    }
}
