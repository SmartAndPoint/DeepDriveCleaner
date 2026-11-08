//! `ddc disk docker`: what Docker Desktop keeps in its disk image, and what
//! a clean of it would free (story "Docker Desktop: what is inside its disk
//! image, and what a clean frees"; menu bar ADR §6 item 10). It runs the
//! clean's readiness probe and one `docker system df -v`, takes no lock,
//! saves nothing and removes nothing. Every answer docker gives exits 0.

use std::io::{IsTerminal as _, Write as _};
use std::process::ExitCode;

use anyhow::Context as _;
use ddc_core::docker;
use ddc_core::env::Env;
use ddc_core::exec::SystemCommands;
use ddc_core::platform::signals;

use crate::cli::DockerArgs;
use crate::render;

pub fn run(args: &DockerArgs) -> anyhow::Result<ExitCode> {
    let cancel = signals::install().context("cannot install the signal handler")?;
    let env = Env::from_process(args.data_dir.clone())?;
    let commands = SystemCommands::new(&env);
    let contents = docker::contents(&commands, cancel);
    // A stop ends it by its signal, with nothing printed.
    if let Some(sig) = signals::received() {
        ddc_core::holders::end_by_signal(sig);
    }
    let Some(contents) = contents else {
        eprintln!("ddc: stopped before docker answered");
        return Ok(ExitCode::FAILURE);
    };

    let mut stdout = std::io::stdout().lock();
    let written = if args.json {
        serde_json::to_writer_pretty(&mut stdout, &contents)
            .map_err(std::io::Error::from)
            .and_then(|()| writeln!(stdout))
    } else {
        let color = render::use_color(std::io::stdout().is_terminal(), Env::no_color());
        render::docker(&mut stdout, &contents, &render::Style { color, all: false })
    };
    match written {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(ExitCode::SUCCESS),
        other => other.map(|()| ExitCode::SUCCESS).map_err(Into::into),
    }
}
