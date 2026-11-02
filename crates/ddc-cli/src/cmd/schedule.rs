//! `ddc schedule install|uninstall|status`.

use ddc_core::platform::Canonical as _;
use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

use anyhow::Context as _;
use ddc_core::env::Env;
use ddc_core::settings;
use ddc_core::store;

use crate::cli::{IntervalArg, ScheduleAction};
use crate::exit;
use crate::platform::scheduler::{ScheduleSpec, Scheduler};
use crate::size::human;

pub fn run(action: ScheduleAction) -> anyhow::Result<ExitCode> {
    let env = Env::from_process(None)?;
    let scheduler = crate::platform::scheduler(&env);
    match action {
        ScheduleAction::Install { interval, threshold } => {
            let exe = std::env::current_exe()?.canonical().context("cannot resolve the ddc binary")?;
            let (mut out, mut err) = (std::io::stdout().lock(), std::io::stderr().lock());
            return install_with(interval, threshold, &exe, &env, scheduler.as_ref(), &mut out, &mut err);
        }
        ScheduleAction::Uninstall => {
            let existed = scheduler.uninstall()?;
            println!("{}", if existed { "Removed the background scan." } else { "No background scan was installed." });
        }
        ScheduleAction::Status => {
            let current = std::env::current_exe().ok();
            let Some(lines) = scheduler.status(current.as_deref()) else {
                println!("Not installed. Run `ddc schedule install`.");
                return Ok(ExitCode::SUCCESS);
            };
            for line in lines {
                println!("{line}");
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

/// Installs the CLI's own schedule, unless the app runs the scheduled scan
/// (ADR "Menu bar app: Swift shell over the ddc CLI", §5): one scheduler at a time.
pub fn install_with(
    interval: IntervalArg,
    threshold: Option<u64>,
    exe: &Path,
    env: &Env,
    scheduler: &dyn Scheduler,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> anyhow::Result<ExitCode> {
    if let Some(record) = settings::load(env)?.scheduler {
        if record.app.exists() {
            writeln!(
                err,
                "ddc schedule install: {} runs the scheduled scan; turn its scans off in its Settings first",
                record.app.display()
            )?;
            return Ok(ExitCode::from(exit::REFUSED));
        }
        settings::clear_scheduler(env)?;
        writeln!(out, "Note: {} is gone; cleared its scheduler record.", record.app.display())?;
    }
    let spec = ScheduleSpec { exe: exe.to_path_buf(), interval, threshold, log_dir: env.log_dir().to_path_buf() };
    let installed = scheduler.install(&spec)?;
    let every = match interval {
        IntervalArg::Daily => "daily",
        IntervalArg::Weekly => "weekly",
    };
    let from = threshold.map_or_else(|| "the settings' threshold (10.0 GiB unless set)".to_owned(), human);
    writeln!(out, "Installed {} ({every}, notify from {from}).", installed.path.display())?;
    writeln!(out, "It only scans; clean with `ddc clean`. Test it now: {}", installed.try_now)?;
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use ddc_core::settings::{SchedulerOwner, SchedulerRecord, Settings};

    use super::*;
    use crate::platform::launchd::{self, Launchctl as _, Launchd, fake::FakeLaunchctl};

    struct Fx {
        tmp: tempfile::TempDir,
        env: Env,
    }

    impl Fx {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let home = tmp.path().join("home");
            std::fs::create_dir_all(&home).unwrap();
            let data = tmp.path().join("data");
            let env = Env::with_paths(&home, &data, &data.join("logs"), SystemTime::now()).unwrap();
            Self { tmp, env }
        }

        /// The app as the record names it; `exists` makes its bundle.
        fn record(&self, exists: bool) -> std::path::PathBuf {
            let app = self.tmp.path().join("DeepDriveCleaner.app");
            if exists {
                std::fs::create_dir_all(&app).unwrap();
            }
            let s = Settings {
                notify_threshold: Some(5 << 30),
                scheduler: Some(SchedulerRecord { by: SchedulerOwner::App, app: app.clone() }),
                ..Settings::default()
            };
            std::fs::create_dir_all(self.env.data_dir()).unwrap();
            std::fs::write(settings::path(&self.env), serde_json::to_vec(&s).unwrap()).unwrap();
            app
        }

        /// The exit code, stdout and stderr.
        fn install(&self, exe: &Path, lc: &FakeLaunchctl) -> (ExitCode, String, String) {
            let (mut out, mut err) = (Vec::new(), Vec::new());
            let launchd = Launchd { home: self.env.home().to_path_buf(), uid: 501, lc };
            let code = install_with(IntervalArg::Daily, None, exe, &self.env, &launchd, &mut out, &mut err).unwrap();
            (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
        }

        fn plist(&self) -> Option<String> {
            std::fs::read_to_string(launchd::plist_path(self.env.home())).ok()
        }
    }

    const EXE: &str = "/opt/homebrew/bin/deepdrivecleaner";

    #[test]
    fn a_live_app_scheduler_refuses_without_touching_launchd() {
        let fx = Fx::new();
        let app = fx.record(true);
        let lc = FakeLaunchctl::default();

        let (code, _, err) = fx.install(Path::new(EXE), &lc);

        assert_eq!(code, ExitCode::from(exit::REFUSED));
        assert!(err.contains(&app.display().to_string()), "names the app: {err}");
        assert!(lc.0.borrow().is_empty(), "no launchctl call");
        assert!(fx.plist().is_none(), "no plist");
    }

    #[test]
    fn a_stale_record_is_cleared_and_the_agent_installed() {
        let fx = Fx::new();
        let app = fx.record(false);
        let lc = FakeLaunchctl::default();

        let (code, out, _) = fx.install(Path::new(EXE), &lc);

        assert_eq!(code, ExitCode::SUCCESS);
        assert!(out.contains(&format!("{} is gone", app.display())), "{out}");
        let s = settings::load(&fx.env).unwrap();
        assert_eq!((s.scheduler, s.notify_threshold), (None, Some(5 << 30)), "only the record is cleared");
        assert!(lc.loaded(&launchd::target(501)));
        let plist = fx.plist().unwrap();
        assert!(!plist.contains("--threshold") && !plist.contains("AssociatedBundleIdentifiers"));
    }

    #[test]
    fn installed_from_inside_the_app_it_is_listed_under_the_app() {
        let fx = Fx::new();
        let exe = fx.tmp.path().join("X.app/Contents/Helpers/deepdrivecleaner");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "binary").unwrap();

        let (code, ..) = fx.install(&exe, &FakeLaunchctl::default());

        assert_eq!(code, ExitCode::SUCCESS);
        assert!(fx.plist().unwrap().contains("<string>com.smartandpoint.deepdrivecleaner</string>\n    </array>"));
    }
}
