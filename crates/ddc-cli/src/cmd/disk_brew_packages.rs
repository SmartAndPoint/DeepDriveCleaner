//! `ddc disk brew-packages`: the Homebrew formulae installed on request,
//! their kegs measured, what they need and what needs them (story "Whole
//! disk: Homebrew packages, their sizes, and brew uninstall"; menu bar ADR
//! §6 item 10). Read-only and lock-free: it runs `brew info` and `brew
//! services list`, saves nothing and removes nothing.

use std::io::{IsTerminal as _, Write as _};
use std::process::ExitCode;

use ddc_core::brew::listing::{self, ListError};
use ddc_core::disk::Sources;
use ddc_core::env::Env;
use ddc_core::exec::SystemCommands;
use ddc_core::settings::ScanFlags;

use crate::cli::BrewPackagesArgs;
use crate::render;

pub fn run(args: &BrewPackagesArgs) -> anyhow::Result<ExitCode> {
    let env = Env::from_process(args.data_dir.clone())?;
    // The skips only: a keg at, above or under one is not measured.
    let resolved = super::resolve(&env, ScanFlags::default())?;
    super::note_missing(&resolved.missing);
    let sources = args.system_root.as_deref().map_or_else(Sources::macos, Sources::under);
    let commands = SystemCommands::new(&env);
    let listed = match listing::list(&commands, &sources, env.home(), &resolved.options.user_skips) {
        Ok(listed) => listed,
        Err(ListError::Probe(reason)) => {
            eprintln!("ddc: brew could not list its packages: {}", render::refusal(reason));
            return Ok(ExitCode::FAILURE);
        }
        Err(e) => {
            eprintln!("ddc: {e}");
            return Ok(ExitCode::FAILURE);
        }
    };

    let mut stdout = std::io::stdout().lock();
    let written = if args.json {
        serde_json::to_writer_pretty(&mut stdout, &listed).map_err(std::io::Error::from).and_then(|()| writeln!(stdout))
    } else {
        let color = render::use_color(std::io::stdout().is_terminal(), Env::no_color());
        render::brew_packages(&mut stdout, &listed, env.home(), &render::Style { color, all: false })
    };
    match written {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(ExitCode::SUCCESS),
        other => other.map(|()| ExitCode::SUCCESS).map_err(Into::into),
    }
}
