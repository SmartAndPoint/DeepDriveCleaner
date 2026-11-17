//! `ddc settings set agents_clean_safe on|off` (story "Settings: let coding
//! agents clean safe items"; ADR "Agent footprint", §7, note of 2026-11-15;
//! menu bar ADR §6 item 6): the one writer that changes the key. It takes no
//! ddc lock: it holds `settings.lock` for a moment, and a change is
//! journaled. At a terminal `on` asks first; without one it needs the app's
//! hidden `--from-app`. `off` never asks.

use std::io::{IsTerminal as _, Write as _};
use std::process::ExitCode;

use ddc_core::env::Env;
use ddc_core::exec::FileJournal;
use ddc_core::platform::procs;
use ddc_core::settings;

use crate::cli::{OnOff, SettingKey, SettingsSetArgs};
use crate::{disclaimer, exit, prompt};

pub fn run(args: &SettingsSetArgs) -> anyhow::Result<ExitCode> {
    if procs::is_root() {
        return refuse("refusing to run as root; run it as your user");
    }
    let SettingKey::AgentsCleanSafe = args.key;
    let on = args.value == OnOff::On;
    let env = Env::from_process(args.data_dir.clone())?;
    // Read once, only to skip the question when it is already on. Then
    // nothing is written: an `off` landing in between must not be undone by
    // an `on` nobody confirmed. Anything written is decided under settings.lock.
    let already = settings::load(&env)?.agents_clean_safe;
    if on && !args.from_app {
        if already {
            return say(true, false);
        }
        if !std::io::stdin().is_terminal() {
            return refuse("switching this on needs you at a terminal, or DeepDriveCleaner's Settings");
        }
        if !ask()? {
            return Ok(ExitCode::FAILURE);
        }
    }
    let switched = settings::set_agents_clean_safe(&env, on, || FileJournal::open(&env))?;
    if switched.unlocked {
        eprintln!("ddc: the file system of {} takes no locks; wrote it without one", settings::path(&env).display());
    }
    say(switched.now, switched.changed())
}

fn say(now: bool, changed: bool) -> anyhow::Result<ExitCode> {
    let mut out = std::io::stdout().lock();
    let state = if now { "on" } else { "off" };
    let unchanged = if changed { "" } else { " (unchanged)" };
    match writeln!(out, "agents_clean_safe: {state}{unchanged}").and_then(|()| out.flush()) {
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        other => other?,
    }
    Ok(ExitCode::SUCCESS)
}

/// At a terminal: the risk, the disclaimer and one question, default No.
fn ask() -> anyhow::Result<bool> {
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "Coding agents could then remove the safe build caches, like __pycache__, inside their own project \
         without asking you. Never a .venv, a worktree or a shared cache. This lets a program remove files \
         without asking you each time; the disclaimer: {}",
        disclaimer::URL
    )?;
    if prompt::confirm_area_action("Let coding agents clean safe items?", std::io::stdin().lock(), &mut out)? {
        return Ok(true);
    }
    writeln!(out, "Nothing changed.")?;
    Ok(false)
}

fn refuse(why: &str) -> anyhow::Result<ExitCode> {
    eprintln!("ddc settings set: {why}");
    Ok(ExitCode::from(exit::REFUSED))
}
