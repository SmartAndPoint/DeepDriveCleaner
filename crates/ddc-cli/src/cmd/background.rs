//! `ddc scan --background`: the scheduled scan. Scan only — it has no code
//! path to the cleanup executor — silent, low priority, skips TCC areas. With
//! `--notify json` it prints the decision for the app instead of notifying.

use std::fs::OpenOptions;
use std::io::Write;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use ddc_core::env::Env;
use ddc_core::notify::{NOTIFY_SCHEMA_VERSION, NotifyReport, NotifyStatus, SkipCause};
use ddc_core::report::volumes::StatFs;
use ddc_core::report::{self, ScanOptions};
use ddc_core::scan::{Progress, WalkOptions};
use ddc_core::settings::{self, ScanFlags};
use ddc_core::store::{self, LockError, LockMode};

use crate::cli::ScanArgs;
use crate::notify::{self, Config, Facts, Notifier};
use crate::render;
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

/// The notification threshold when neither `--threshold` nor `settings.json` sets one.
const DEFAULT_THRESHOLD: u64 = 10 << 30;

/// The walk of a scheduled scan: three threads, so it stays in the background.
fn background_walk() -> WalkOptions {
    WalkOptions { threads: Some(3), ..WalkOptions::default() }
}

/// The dispatcher hands in how to notify and how to read volumes, so this
/// module never names a way to run a program. A platform without the
/// `notifications` capability decides but shows nothing (ADR "Platform seam:
/// one platform module per OS with declared capabilities", §3).
pub fn run(args: &ScanArgs, notifier: &dyn Notifier, statfs: &dyn StatFs) -> anyhow::Result<ExitCode> {
    let env = Env::from_process(args.data_dir.clone())?;
    let notifier =
        ddc_core::platform::CAPABILITIES.contains(&ddc_core::platform::Capability::Notifications).then_some(notifier);
    run_with(args, &env, notifier, statfs, &mut std::io::stdout().lock())
}

/// `notifier` is `None` where the platform cannot show a notification.
pub fn run_with(
    args: &ScanArgs,
    env: &Env,
    notifier: Option<&dyn Notifier>,
    statfs: &dyn StatFs,
    out: &mut dyn Write,
) -> anyhow::Result<ExitCode> {
    let json = args.notify.is_some();
    let _lock = match store::try_lock(env, LockMode::Shared) {
        Ok(lock) => lock,
        Err(LockError::Busy(_)) => {
            log(env, "skipped: ddc clean is running");
            if json {
                print(out, &NotifyStatus::Skipped { reason: SkipCause::Busy })?;
            }
            return Ok(ExitCode::SUCCESS);
        }
        Err(e) => return Err(e.into()),
    };
    let settings = settings::load(env)?;
    let given_roots = !args.paths.is_empty();
    let resolved = settings.resolve(
        env,
        &ScanFlags {
            paths: args.paths.clone(),
            excludes: args.exclude.clone(),
            skips: args.skip.clone(),
            min_age_days: args.min_age,
        },
    )?;
    for path in &resolved.missing {
        log(env, &format!("{} in settings.json does not exist; skipped", path.display()));
    }
    if !resolved.dropped_excludes.is_empty() {
        let names: Vec<String> = resolved.dropped_excludes.iter().map(|p| p.display().to_string()).collect();
        log(env, &format!("--exclude replaces settings.json's excludes; no longer protected: {}", names.join(", ")));
    }
    let config = Config {
        threshold: args.threshold.or(settings.notify_threshold).unwrap_or(DEFAULT_THRESHOLD),
        low_disk: settings.notify_low_disk.unwrap_or(true),
    };
    let opts = ScanOptions {
        // With Full Disk Access granted to this binary nothing prompts.
        skip: if report::full_disk_access(env) {
            Vec::new()
        } else {
            PROMPTING_AREAS.iter().map(|rel| env.home().join(rel)).collect()
        },
        walk: background_walk(),
        ..resolved.options
    };
    let report = report::scan_with(env, &opts, &Progress::default(), statfs)?;
    store::write_last_scan(env, &report)?;
    for root in report.scan_roots.iter().filter(|r| !report.volumes.iter().any(|v| v.roots.contains(r))) {
        log(env, &format!("no volume information for {}; low disk is not checked there", root.display()));
    }

    // The scan's start: a snooze ends for scans that start after it.
    let now = env.now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let facts = Facts::from_report(&report, now);
    let decision = notify::decide(&facts, &config, &notify::load(env));
    // Logged whether or not anything shows it, so the text is checked where no notifier exists.
    let roots = if super::own_roots(given_roots, &report, env, &settings) { &report.scan_roots[..] } else { &[] };
    let text = notify::message(&decision, &render::clean_command(roots, &report.home));
    if decision.notify {
        log(env, &format!("notification: {text}"));
    }
    let mut notified = false;
    if json {
        // The app shows it and records it with `notify ack`; nothing is written here.
        print(out, &NotifyStatus::Scanned(decision.clone()))?;
    } else if let Some(notifier) = notifier.filter(|_| !args.no_notify && decision.notify) {
        match notifier.notify("DeepDriveCleaner", &text) {
            Ok(()) => {
                notified = true;
                // Shown already: a state we cannot save must not fail the run.
                if let Err(e) = notify::ack(env, facts.reclaimable, now) {
                    log(env, &format!("cannot save the notification state: {e}"));
                }
            }
            Err(e) => log(env, &format!("notification failed: {e}")),
        }
    }
    let why = match (decision.reason, &decision.volume) {
        (Some(reason), Some(volume)) => format!(" ({reason:?} on {})", volume.display()),
        _ => String::new(),
    };
    log(
        env,
        &format!(
            "scanned {} files in {:.1}s; can free ~{}; inaccessible {}; notified {notified}{why}",
            report.totals.files,
            report.duration_ms as f64 / 1000.0,
            human(facts.reclaimable),
            report.inaccessible.len()
        ),
    );
    Ok(ExitCode::SUCCESS)
}

fn print(out: &mut dyn Write, status: &NotifyStatus) -> std::io::Result<()> {
    let report = NotifyReport { schema_version: NOTIFY_SCHEMA_VERSION, status: status.clone() };
    serde_json::to_writer(&mut *out, &report)?;
    writeln!(out)?;
    out.flush()
}

fn log(env: &Env, line: &str) {
    let _ = std::fs::create_dir_all(env.log_dir());
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(env.log_dir().join("background.log")) {
        let ts = humantime::format_rfc3339_seconds(SystemTime::now());
        let _ = writeln!(f, "{ts} {line}");
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::io;
    use std::path::Path;

    use clap::Parser as _;
    use ddc_core::notify::{Decision, NotifyReason};
    use ddc_core::platform::statfs::VolumeStats;
    // A build with `SUPPORTED` false refuses `background`, whose decision reads the volume.
    use ddc_loadgen::unsupported;

    use super::*;
    use crate::cli::{Cli, Command};

    /// Records every notification instead of showing it.
    #[derive(Default)]
    struct Recorder(RefCell<Vec<String>>);

    impl Notifier for Recorder {
        fn notify(&self, _: &str, body: &str) -> io::Result<()> {
            self.0.borrow_mut().push(body.to_owned());
            Ok(())
        }
    }

    /// Every scan root on one volume with this much free.
    struct Disk {
        free: u64,
        total: u64,
    }

    impl StatFs for Disk {
        fn statfs(&self, _: &Path) -> io::Result<VolumeStats> {
            Ok(VolumeStats {
                fs_type: "apfs".into(),
                mount_point: "/Volumes/Test".into(),
                available_bytes: self.free,
                total_bytes: self.total,
                free_bytes: self.free,
                mounted_from: "/dev/disk9s1".into(),
            })
        }
    }

    const ROOMY: Disk = Disk { free: 50, total: 100 };

    fn args(extra: &[&str]) -> ScanArgs {
        match Cli::try_parse_from([&["deepdrivecleaner", "scan", "--background"], extra].concat()).unwrap().command {
            Command::Scan(args) => args,
            _ => unreachable!(),
        }
    }

    fn fixture() -> (tempfile::TempDir, Env) {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir_all(home.join("p/__pycache__")).unwrap();
        std::fs::write(home.join("p/__pycache__/m.pyc"), vec![1u8; 50_000]).unwrap();
        let env =
            Env::with_paths(&home, &tmp.path().join("data"), &tmp.path().join("logs"), SystemTime::now()).unwrap();
        (tmp, env)
    }

    fn decision(out: &[u8]) -> Decision {
        match serde_json::from_slice::<NotifyReport>(out).unwrap().status {
            NotifyStatus::Scanned(d) => d,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_background_walk_uses_three_threads() {
        assert_eq!(background_walk().threads, Some(3));
    }

    #[test]
    fn json_prints_the_decision_and_never_notifies_or_records_it() {
        if unsupported("json_prints_the_decision_and_never_notifies_or_records_it") {
            return;
        }
        let (_tmp, env) = fixture();
        let recorder = Recorder::default();
        let mut out = Vec::new();

        run_with(&args(&["--notify", "json", "--threshold", "1K"]), &env, Some(&recorder), &ROOMY, &mut out).unwrap();

        let d = decision(&out);
        assert_eq!((d.notify, d.reason), (true, Some(NotifyReason::Threshold)));
        assert!(recorder.0.borrow().is_empty(), "the app shows it");
        assert!(!env.data_dir().join("notify-state.json").exists(), "the app records it with `notify ack`");
        assert!(env.data_dir().join("last-scan.json").exists(), "the app reads the scan");
    }

    #[test]
    fn without_json_it_notifies_once_and_records_it() {
        if unsupported("without_json_it_notifies_once_and_records_it") {
            return;
        }
        let (_tmp, env) = fixture();
        let recorder = Recorder::default();
        let mut out = Vec::new();

        run_with(&args(&["--threshold", "1K"]), &env, Some(&recorder), &ROOMY, &mut out).unwrap();
        run_with(&args(&["--threshold", "1K"]), &env, Some(&recorder), &ROOMY, &mut out).unwrap();
        run_with(&args(&["--threshold", "1K", "--no-notify"]), &env, Some(&recorder), &ROOMY, &mut out).unwrap();

        assert_eq!(recorder.0.borrow().len(), 1, "the same amount again: no repeat");
        assert!(out.is_empty(), "silent");
        assert!(notify::load(&env).last_at.is_some());
    }

    /// A platform without the `notifications` capability decides, shows
    /// nothing and records nothing as shown (ADR "Platform seam: one
    /// platform module per OS with declared capabilities", §3).
    #[test]
    fn without_a_notifier_it_decides_and_shows_nothing() {
        if unsupported("without_a_notifier_it_decides_and_shows_nothing") {
            return;
        }
        let (_tmp, env) = fixture();
        let mut out = Vec::new();
        run_with(&args(&["--threshold", "1K"]), &env, None, &ROOMY, &mut out).unwrap();
        assert!(out.is_empty(), "silent");
        assert!(notify::load(&env).last_at.is_none(), "nothing was shown");
        assert!(env.data_dir().join("last-scan.json").exists(), "the scan ran");
    }

    /// The text is logged whether or not a notifier shows it, and its
    /// `ddc clean` names the paths the scan was given.
    #[test]
    fn the_notification_is_logged_with_the_clean_of_the_scanned_paths() {
        if unsupported("the_notification_is_logged_with_the_clean_of_the_scanned_paths") {
            return;
        }
        let (tmp, _) = fixture();
        let root = tmp.path().canonicalize().unwrap();
        let env =
            Env::with_paths(&root.join("home"), &root.join("data"), &root.join("logs"), SystemTime::now()).unwrap();
        let p = root.join("home/p");
        let mut out = Vec::new();
        run_with(&args(&["--threshold", "1K", p.to_str().unwrap()]), &env, None, &ROOMY, &mut out).unwrap();
        run_with(&args(&["--threshold", "1K", "--no-notify"]), &env, None, &ROOMY, &mut out).unwrap();
        let log = std::fs::read_to_string(env.log_dir().join("background.log")).unwrap();
        let lines: Vec<&str> = log.lines().filter(|l| l.contains(" notification: ")).collect();
        assert_eq!(lines.len(), 2, "{log}");
        assert!(lines[0].ends_with("Run `ddc clean ~/p` to review."), "{log}");
        assert!(lines[1].ends_with("Run `ddc clean` to review."), "{log}");
    }

    #[test]
    fn a_volume_low_on_space_notifies_and_is_named() {
        if unsupported("a_volume_low_on_space_notifies_and_is_named") {
            return;
        }
        let (_tmp, env) = fixture();
        let mut out = Vec::new();

        let low = Disk { free: 5, total: 100 };
        run_with(&args(&["--notify", "json", "--threshold", "100G"]), &env, Some(&Recorder::default()), &low, &mut out)
            .unwrap();

        let d = decision(&out);
        assert_eq!((d.notify, d.reason), (true, Some(NotifyReason::LowDisk)));
        assert_eq!((d.volume.as_deref(), d.free, d.total), (Some(Path::new("/Volumes/Test")), Some(5), Some(100)));
    }

    #[test]
    fn low_disk_can_be_switched_off_in_the_settings() {
        if unsupported("low_disk_can_be_switched_off_in_the_settings") {
            return;
        }
        let (_tmp, env) = fixture();
        std::fs::create_dir_all(env.data_dir()).unwrap();
        std::fs::write(env.data_dir().join("settings.json"), r#"{"schema_version": 1, "notify_low_disk": false}"#)
            .unwrap();
        let mut out = Vec::new();

        let low = Disk { free: 5, total: 100 };
        run_with(&args(&["--notify", "json", "--threshold", "100G"]), &env, Some(&Recorder::default()), &low, &mut out)
            .unwrap();

        assert!(!decision(&out).notify);
    }

    #[test]
    fn the_threshold_flag_wins_over_the_settings() {
        if unsupported("the_threshold_flag_wins_over_the_settings") {
            return;
        }
        let (_tmp, env) = fixture();
        std::fs::create_dir_all(env.data_dir()).unwrap();
        let settings = |threshold: u64| {
            let json = format!(r#"{{"schema_version": 1, "notify_threshold": {threshold}, "notify_low_disk": false}}"#);
            std::fs::write(env.data_dir().join("settings.json"), json).unwrap();
        };
        let notifies = |extra: &[&str]| {
            let mut out = Vec::new();
            run_with(
                &args(&[&["--notify", "json"], extra].concat()),
                &env,
                Some(&Recorder::default()),
                &ROOMY,
                &mut out,
            )
            .unwrap();
            decision(&out).notify
        };

        settings(1024);
        assert!(notifies(&[]), "the app passes no --threshold: the settings' threshold applies");
        assert!(!notifies(&["--threshold", "100G"]), "an explicit --threshold (a 0.1.0 agent) wins");
        settings(100 << 30);
        assert!(notifies(&["--threshold", "1K"]));
        assert!(!notifies(&[]));
    }
}
