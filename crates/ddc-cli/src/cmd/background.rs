//! `ddc scan --background`: the scheduled scan. Scan only — it has no code
//! path to the cleanup executor — silent, low priority, skips TCC areas.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ddc_core::env::Env;
use ddc_core::report::{self, ScanOptions};
use ddc_core::scan::{Progress, WalkOptions};
use ddc_core::store::{self, LockError, LockMode};
use ddc_core::sys::statfs;

use crate::cli::ScanArgs;
use crate::notify::{self, Facts, NotifyState};
use crate::size::human;

/// Folders where macOS asks the user before an app may read them (folder
/// access, other apps' data, contacts, calendars, …). A LaunchAgent must never
/// trigger such a prompt: the read blocks until someone answers it.
const PROMPTING_AREAS: &[&str] = &[
    "Desktop",
    "Documents",
    "Downloads",
    "Pictures",
    "Movies",
    "Music",
    "Library/Containers",
    "Library/Group Containers",
    "Library/Mail",
    "Library/Messages",
    "Library/Calendars",
    "Library/Reminders",
    "Library/Application Support/AddressBook",
    "Library/Application Support/CallHistoryDB",
    "Library/Application Support/MobileSync",
];

pub fn run(args: &ScanArgs) -> anyhow::Result<ExitCode> {
    let env = Env::from_process(args.data_dir.clone())?;
    let _lock = match store::try_lock(&env, LockMode::Shared) {
        Ok(lock) => lock,
        Err(LockError::Busy(_)) => {
            log(&env, "skipped: ddc clean is running");
            return Ok(ExitCode::SUCCESS);
        }
        Err(e) => return Err(e.into()),
    };
    let opts = ScanOptions {
        roots: args.paths.clone(),
        excludes: args.exclude.clone(),
        min_age: Duration::from_secs(args.min_age * 86_400),
        // With Full Disk Access granted to this binary nothing prompts.
        skip: if report::full_disk_access(&env) {
            Vec::new()
        } else {
            PROMPTING_AREAS.iter().map(|rel| env.home().join(rel)).collect()
        },
        walk: WalkOptions { threads: Some(3), ..WalkOptions::default() },
        ..ScanOptions::default()
    };
    let report = report::scan(&env, &opts, &Progress::default())?;
    store::write_last_scan(&env, &report)?;

    let vol = statfs::statfs(env.home()).ok();
    let facts = Facts {
        reclaimable: report.totals.selected_reclaimable_bytes,
        free: vol.as_ref().map(|v| v.available_bytes),
        total: vol.as_ref().map(|v| v.total_bytes),
        now: SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
    };
    let mut notified = false;
    if !args.no_notify && notify::decide(&facts, args.threshold, &notify::load(&env)) {
        match notify::send("DeepDriveCleaner", &notify::message(&facts)) {
            Ok(()) => {
                notified = true;
                // Shown already: a state we cannot save must not fail the run.
                if let Err(e) =
                    notify::save(&env, &NotifyState { last_at: Some(facts.now), last_bytes: facts.reclaimable })
                {
                    log(&env, &format!("cannot save the notification state: {e}"));
                }
            }
            Err(e) => log(&env, &format!("notification failed: {e}")),
        }
    }
    log(
        &env,
        &format!(
            "scanned {} files in {:.1}s; can free ~{}; inaccessible {}; notified {notified}",
            report.totals.files,
            report.duration_ms as f64 / 1000.0,
            human(facts.reclaimable),
            report.inaccessible.len()
        ),
    );
    Ok(ExitCode::SUCCESS)
}

fn log(env: &Env, line: &str) {
    let _ = std::fs::create_dir_all(env.log_dir());
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(env.log_dir().join("background.log")) {
        let ts = humantime::format_rfc3339_seconds(SystemTime::now());
        let _ = writeln!(f, "{ts} {line}");
    }
}
