//! `ddc schedule install|uninstall|status`.

use std::process::ExitCode;

use anyhow::Context as _;
use ddc_core::env::Env;
use ddc_core::store;
use ddc_core::sys::procs;

use crate::cli::{IntervalArg, ScheduleAction};
use crate::launchd::{self, AgentSpec, Launchctl as _, ProgramState, SystemLaunchctl};
use crate::size::human;

pub fn run(action: ScheduleAction) -> anyhow::Result<ExitCode> {
    let env = Env::from_process(None)?;
    let uid = procs::uid();
    let lc = SystemLaunchctl;
    match action {
        ScheduleAction::Install { interval, threshold } => {
            let exe = std::env::current_exe()?.canonicalize().context("cannot resolve the ddc binary")?;
            let spec = AgentSpec { exe, interval, threshold, log_dir: env.log_dir().to_path_buf() };
            let path = launchd::install(&spec, env.home(), uid, &lc)?;
            let every = match interval {
                IntervalArg::Daily => "daily",
                IntervalArg::Weekly => "weekly",
            };
            println!("Installed {} ({every}, notify from {}).", path.display(), human(threshold));
            println!(
                "It only scans; clean with `ddc clean`. Test it now: launchctl kickstart -k {}",
                launchd::target(uid)
            );
        }
        ScheduleAction::Uninstall => {
            let existed = launchd::uninstall(env.home(), uid, &lc)?;
            println!("{}", if existed { "Removed the background scan." } else { "No background scan was installed." });
        }
        ScheduleAction::Status => {
            let path = launchd::plist_path(env.home());
            let Ok(text) = std::fs::read_to_string(&path) else {
                println!("Not installed. Run `ddc schedule install`.");
                return Ok(ExitCode::SUCCESS);
            };
            println!("Agent:   {}", path.display());
            println!("Loaded:  {}", if lc.loaded(&launchd::target(uid)) { "yes" } else { "no" });
            let current = std::env::current_exe().ok();
            match launchd::installed_program(&text) {
                Some(p) => match launchd::program_state(&p, current.as_deref()) {
                    ProgramState::Missing => println!("Binary:  {} (missing! reinstall)", p.display()),
                    ProgramState::Other => println!("Binary:  {} (not this ddc)", p.display()),
                    ProgramState::Current => println!("Binary:  {}", p.display()),
                },
                None => println!("Binary:  unreadable plist"),
            }
            match store::read_last_scan(&env)? {
                Some(r) => {
                    println!("Last scan: {} — can free ~{}", r.generated_at, human(r.totals.selected_reclaimable_bytes))
                }
                None => println!("Last scan: none yet"),
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}
