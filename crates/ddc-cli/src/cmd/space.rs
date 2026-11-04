//! `ddc space`: whether in-use space on the home folder's volume grew since
//! the last scan (story "Popover: say when used space grew since the last
//! scan"; ADR "Menu bar app: Swift shell over the ddc CLI", §6 item 13). One
//! `statfs`, no walk and no lock: it writes only its samples, `space.json`.
//! The rule is the core's (`ddc_core::space`); this prints it.

use std::io::{self, Write};
use std::process::ExitCode;

use anyhow::Context as _;
use ddc_core::env::Env;
use ddc_core::report::volumes::{StatFs, SystemStatFs};
use ddc_core::settings;
use ddc_core::space::{self, BaselineSource, SpaceReport, SpaceState};

use crate::cli::SpaceArgs;
use crate::notify::volume_name;
use crate::render::{age, mid_sentence};
use crate::size::human;

pub fn run(args: &SpaceArgs) -> anyhow::Result<ExitCode> {
    let env = Env::from_process(args.data_dir.clone())?;
    run_with(args, &env, &SystemStatFs, &mut io::stdout().lock(), &mut io::stderr().lock())
}

/// `out` takes the result, `err` the warnings: an unreadable
/// `settings.json` (low disk then counts, since `space` changes nothing)
/// and a `space.json` that cannot be written. Only a `statfs` that fails is
/// an error.
pub fn run_with(
    args: &SpaceArgs,
    env: &Env,
    statfs: &dyn StatFs,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> anyhow::Result<ExitCode> {
    let low_disk = match settings::load(env) {
        Ok(s) => s.notify_low_disk != Some(false),
        Err(e) => {
            let _ = writeln!(err, "ddc: warning: {e}; the low-disk check stays on");
            true
        }
    };
    let report = space::check(env, statfs, low_disk)
        .with_context(|| format!("cannot read the free space of {}", env.home().display()))?;
    if let Err(e) = space::record(env, &report) {
        let _ =
            writeln!(err, "ddc: warning: cannot save {}: {e}", env.data_dir().join(ddc_core::store::SPACE).display());
    }
    let written = if args.json {
        serde_json::to_writer_pretty(&mut *out, &report).map_err(io::Error::from).and_then(|()| writeln!(out))
    } else {
        writeln!(out, "{}", sentence(&report))
    };
    match written {
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => Ok(ExitCode::SUCCESS),
        other => other.map(|()| ExitCode::SUCCESS).map_err(Into::into),
    }
}

/// One sentence for the state.
fn sentence(r: &SpaceReport) -> String {
    let name = volume_name(&r.volume.mount_point);
    // "than at the last scan, 26 h ago"
    let since = r.baseline.as_ref().map(|b| {
        let when = match b.source {
            BaselineSource::Scan => "at the last scan",
            BaselineSource::Clean => "after your clean",
        };
        (when, age(&b.at, &r.checked_at).map(|a| format!(", {a}")).unwrap_or_default())
    });
    let delta = || human(r.in_use_delta.and_then(|d| u64::try_from(d).ok()).unwrap_or(0));
    match (r.state, since) {
        (SpaceState::LowDisk, Some((when, ago))) if r.grew => format!(
            "{name} is {}% full, with {} more in use than {when}{ago}. `ddc scan` shows where.",
            percent_full(r.volume.total_bytes, r.volume.free_bytes),
            delta()
        ),
        (SpaceState::LowDisk, _) => {
            format!("{name} is {}% full.", percent_full(r.volume.total_bytes, r.volume.free_bytes))
        }
        (SpaceState::Grew, Some((when, ago))) => {
            format!("{} more in use on {} than {when}{ago}. `ddc scan` shows where.", delta(), mid_sentence(&name))
        }
        (SpaceState::Quiet, Some((when, _))) => {
            format!("No more in use on {} than {when}.", mid_sentence(&name))
        }
        (SpaceState::Grew | SpaceState::Quiet, None) => "No saved scan to compare with yet: run `ddc scan`.".to_owned(),
    }
}

/// How full a volume is, in whole percent rounded down, as the app's
/// `NotificationText` says it.
fn percent_full(total: u64, free: u64) -> u64 {
    if total == 0 {
        return 0;
    }
    let used = u128::from(total - free.min(total));
    u64::try_from(used * 100 / u128::from(total)).unwrap_or(100)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::{Duration, UNIX_EPOCH};

    use ddc_core::report::volumes::VolumeStats;
    use ddc_core::space::{Baseline, Saved, SpaceVolume, decide};

    use super::*;

    const G: u64 = 1 << 30;
    /// 2026-11-03T10:00:00Z
    const NOW: u64 = 1_793_700_000;

    /// The home's volume, `total` and `free` bytes; `None` cannot be read.
    struct Fake(Option<(u64, u64)>);

    impl StatFs for Fake {
        fn statfs(&self, _: &Path) -> io::Result<VolumeStats> {
            let (total, free) = self.0.ok_or_else(|| io::Error::other("no such volume"))?;
            Ok(VolumeStats {
                fs_type: "apfs".into(),
                mount_point: "/System/Volumes/Data".into(),
                available_bytes: free,
                total_bytes: total,
                free_bytes: free,
                mounted_from: "/dev/disk3s5".into(),
            })
        }
    }

    fn at(secs: u64) -> String {
        humantime::format_rfc3339_seconds(UNIX_EPOCH + Duration::from_secs(secs)).to_string()
    }

    fn volume(total: u64, free: u64) -> SpaceVolume {
        SpaceVolume { mount_point: "/System/Volumes/Data".into(), total_bytes: total, free_bytes: free }
    }

    fn saved(source: BaselineSource, ago: u64, total: u64, free: u64) -> Saved {
        Saved { baseline: Baseline { source, at: at(NOW - ago), free_bytes: free }, total_bytes: total }
    }

    fn text(total: u64, free: u64, base: Option<Saved>, low_disk: bool) -> String {
        sentence(&decide(at(NOW), volume(total, free), base, low_disk))
    }

    #[test]
    fn each_state_says_one_sentence() {
        let scan = |ago, free| Some(saved(BaselineSource::Scan, ago, 500 * G, free));
        assert_eq!(
            text(500 * G, 100 * G, scan(26 * 3600, 298 * G), true),
            "198.0 GiB more in use on the startup disk than at the last scan, 26 h ago. `ddc scan` shows where."
        );
        assert_eq!(
            text(500 * G, 100 * G, Some(saved(BaselineSource::Clean, 300, 500 * G, 130 * G)), true),
            "30.0 GiB more in use on the startup disk than after your clean, 5 min ago. `ddc scan` shows where."
        );
        assert_eq!(
            text(500 * G, 100 * G, scan(3600, 110 * G), true),
            "No more in use on the startup disk than at the last scan."
        );
        assert_eq!(
            text(500 * G, 100 * G, Some(saved(BaselineSource::Clean, 60, 500 * G, 90 * G)), true),
            "No more in use on the startup disk than after your clean."
        );
        assert_eq!(text(500 * G, 100 * G, None, true), "No saved scan to compare with yet: run `ddc scan`.");
        // Under 10 % free: 46.8 of 500 GiB free is 90.6 % full, said as 90 %.
        let low = (468 * G) / 10;
        assert_eq!(text(500 * G, low, None, true), "The startup disk is 90% full.");
        assert_eq!(text(500 * G, low, scan(60, 50 * G), true), "The startup disk is 90% full.");
        assert_eq!(
            text(500 * G, low, scan(3 * 86_400, 80 * G), true),
            "The startup disk is 90% full, with 33.2 GiB more in use than at the last scan, 3 d ago. `ddc scan` shows where."
        );
        // The low-disk check off: growth or nothing.
        assert_eq!(text(500 * G, low, None, false), "No saved scan to compare with yet: run `ddc scan`.");
        assert!(text(500 * G, low, scan(30, 80 * G), false).starts_with("33.2 GiB more in use on the startup disk"));
    }

    #[test]
    fn another_volume_is_named_by_its_path_and_its_age_follows_the_apps_wording() {
        let r = decide(
            at(NOW),
            SpaceVolume { mount_point: "/Volumes/Work".into(), total_bytes: 100 * G, free_bytes: 50 * G },
            Some(saved(BaselineSource::Scan, 30, 100 * G, 80 * G)),
            true,
        );
        assert_eq!(
            sentence(&r),
            "30.0 GiB more in use on /Volumes/Work than at the last scan, just now. `ddc scan` shows where."
        );
        let mut odd = r.clone();
        odd.baseline.as_mut().unwrap().at = "yesterday".into();
        assert_eq!(
            sentence(&odd),
            "30.0 GiB more in use on /Volumes/Work than at the last scan. `ddc scan` shows where."
        );
        assert_eq!(percent_full(100, 0), 100);
        assert_eq!(percent_full(100, 101), 0);
        assert_eq!(percent_full(0, 0), 0);
        assert_eq!(percent_full(1000, 95), 90);
    }

    struct Dirs {
        _tmp: tempfile::TempDir,
        env: Env,
    }

    fn dirs() -> Dirs {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        std::fs::create_dir(&home).unwrap();
        let data = tmp.path().join("data");
        let env = Env::with_paths(&home, &data, &data.join("logs"), UNIX_EPOCH + Duration::from_secs(NOW)).unwrap();
        Dirs { _tmp: tmp, env }
    }

    fn run(env: &Env, json: bool, statfs: &Fake) -> (anyhow::Result<ExitCode>, String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let args = SpaceArgs { json, data_dir: None };
        let code = run_with(&args, env, statfs, &mut out, &mut err);
        (code, String::from_utf8(out).unwrap(), String::from_utf8(err).unwrap())
    }

    #[test]
    fn a_volume_that_cannot_be_read_is_an_error_and_prints_nothing() {
        let d = dirs();
        let (code, out, err) = run(&d.env, true, &Fake(None));
        let e = code.unwrap_err();
        assert!(format!("{e:#}").starts_with("cannot read the free space of "), "{e:#}");
        assert!(format!("{e:#}").ends_with("no such volume"), "{e:#}");
        assert!(out.is_empty() && err.is_empty(), "{out}{err}");
        assert!(!d.env.data_dir().join("space.json").exists());
    }

    #[test]
    fn an_unreadable_settings_file_warns_and_low_disk_counts() {
        let d = dirs();
        std::fs::create_dir_all(d.env.data_dir()).unwrap();
        std::fs::write(d.env.data_dir().join("settings.json"), "{ not json").unwrap();
        let (code, out, err) = run(&d.env, false, &Fake(Some((100 * G, 5 * G))));
        assert_eq!(code.unwrap(), ExitCode::SUCCESS);
        assert_eq!(out, "The startup disk is 95% full.\n");
        assert!(err.starts_with("ddc: warning: malformed settings "), "{err}");
        assert!(err.ends_with("; the low-disk check stays on\n"), "{err}");

        // `notify_low_disk: false` turns it off.
        let off = serde_json::json!({ "schema_version": settings::SETTINGS_SCHEMA_VERSION, "notify_low_disk": false });
        std::fs::write(d.env.data_dir().join("settings.json"), off.to_string()).unwrap();
        let (code, out, err) = run(&d.env, true, &Fake(Some((100 * G, 5 * G))));
        assert_eq!(code.unwrap(), ExitCode::SUCCESS);
        let r: SpaceReport = serde_json::from_str(&out).unwrap();
        assert_eq!((r.state, r.grew, r.baseline), (SpaceState::Quiet, false, None));
        assert!(err.is_empty(), "{err}");
    }

    #[test]
    fn a_samples_file_that_cannot_be_written_is_a_warning() {
        let d = dirs();
        // The data folder is a file: nothing can be written under it.
        std::fs::write(d.env.data_dir(), "").unwrap();
        let (code, out, err) = run(&d.env, true, &Fake(Some((500 * G, 200 * G))));
        assert_eq!(code.unwrap(), ExitCode::SUCCESS);
        let r: SpaceReport = serde_json::from_str(&out).unwrap();
        assert_eq!(r.volume.mount_point, PathBuf::from("/System/Volumes/Data"));
        assert!(err.lines().any(|l| l.starts_with("ddc: warning: cannot save ") && l.contains("space.json")), "{err}");
    }

    /// A reader that went away: the result is lost, the exit is still 0.
    struct Closed;

    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_closed_pipe_is_not_an_error() {
        let d = dirs();
        let args = SpaceArgs { json: false, data_dir: None };
        let code = run_with(&args, &d.env, &Fake(Some((500 * G, 200 * G))), &mut Closed, &mut Vec::new());
        assert_eq!(code.unwrap(), ExitCode::SUCCESS);
    }
}
