//! The scheduled scan as a systemd user timer: one [`Scheduler`] (story
//! "Scheduled scans on Linux through a systemd user timer", DDC-010).
//!
//! Both unit files live in ddc's own folder under the home, which depends on
//! the home alone, so `status` and `uninstall` find them whatever the shell's
//! `XDG_STATE_HOME` or `DDC_DATA_DIR` says. `systemctl --user link` and
//! `enable` by absolute path put them into the user manager's own search
//! path, which a client's environment cannot move. No root is needed.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::scheduler::{
    Installed, ProgramState, ScheduleSpec, Scheduler, Status, Unavailable, program_state, refuse_cargo_target,
    scheduled_program,
};
use crate::cli::IntervalArg;

pub const SERVICE: &str = "deepdrivecleaner-scan.service";
pub const TIMER: &str = "deepdrivecleaner-scan.timer";

/// The states `systemctl --user is-system-running` prints for a manager that
/// runs; `offline` (not booted under systemd), `unknown` and nothing mean
/// there is none ddc can use.
const LIVE: &[&str] = &["initializing", "starting", "running", "degraded", "maintenance", "stopping"];

/// What to try when systemd runs but this shell cannot reach it.
const RUNTIME_DIR_HINT: &str = "If systemd runs but this shell has no XDG_RUNTIME_DIR (an ssh session without \
     pam_systemd), run `export XDG_RUNTIME_DIR=/run/user/$(id -u)` and try again";

/// ddc's folder for its unit files: under the home, never under a variable.
pub fn unit_dir(home: &Path) -> PathBuf {
    home.join(".local/state/deepdrivecleaner/systemd")
}

/// What one schedule runs; the unit texts are a function of it alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitSpec {
    pub exe: PathBuf,
    pub interval: IntervalArg,
    pub threshold: Option<u64>,
    pub data_dir: PathBuf,
    pub pinned: Vec<(String, PathBuf)>,
}

/// One word of a unit file, quoted: `\` and `"` escaped, `%` (a specifier)
/// written `%%`, and in a command line `$` (a variable) written `$$`. `None`
/// for a word with a control character: a newline would start a unit line of
/// its own.
fn quote(word: &str, command: bool) -> Option<String> {
    let mut out = String::from('"');
    for c in word.chars() {
        match c {
            c if c.is_control() => return None,
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' if command => out.push_str("$$"),
            c => out.push(c),
        }
    }
    out.push('"');
    Some(out)
}

/// The words of a line written by [`quote`]: bare words as they are, quoted
/// ones unquoted again. `None` on an unclosed quote.
fn words(line: &str, command: bool) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.next_if_eq(&' ').is_some() {}
        let Some(first) = chars.next() else { return Some(out) };
        let mut w = String::new();
        if first != '"' {
            // A bare word: `scan`, `--background`, a number.
            w.push(first);
            while let Some(c) = chars.next_if(|c| *c != ' ') {
                w.push(c);
            }
            out.push(w);
            continue;
        }
        loop {
            match chars.next()? {
                '"' => break,
                '\\' => w.push(chars.next()?),
                '%' => w.push(chars.next_if_eq(&'%').unwrap_or('%')),
                '$' if command => w.push(chars.next_if_eq(&'$').unwrap_or('$')),
                c => w.push(c),
            }
        }
        out.push(w);
    }
}

/// The service unit; `None` when a path holds a control character, which a
/// unit file cannot carry.
pub fn service_text(spec: &UnitSpec) -> Option<String> {
    let mut args = vec![quote(&spec.exe.to_string_lossy(), true)?, "scan".into(), "--background".into()];
    args.extend(["--data-dir".into(), quote(&spec.data_dir.to_string_lossy(), true)?]);
    if let Some(threshold) = spec.threshold {
        args.extend(["--threshold".into(), threshold.to_string()]);
    }
    let env: String = spec
        .pinned
        .iter()
        .map(|(name, value)| {
            Some(format!("Environment={}\n", quote(&format!("{name}={}", value.to_string_lossy()), false)?))
        })
        .collect::<Option<_>>()?;
    Some(format!(
        "# Written by `ddc schedule install`; `ddc schedule uninstall` removes it.\n\
         [Unit]\n\
         Description=DeepDriveCleaner background scan\n\
         \n\
         [Service]\n\
         Type=oneshot\n\
         ExecStart={}\n\
         {env}\
         Nice=10\n\
         IOSchedulingClass=idle\n",
        args.join(" ")
    ))
}

pub fn timer_text(interval: IntervalArg) -> String {
    let every = match interval {
        IntervalArg::Daily => "daily",
        IntervalArg::Weekly => "weekly",
    };
    format!(
        "# Written by `ddc schedule install`; `ddc schedule uninstall` removes it.\n\
         [Unit]\n\
         Description=DeepDriveCleaner background scan, {every}\n\
         \n\
         [Timer]\n\
         OnCalendar={every}\n\
         Persistent=true\n\
         RandomizedDelaySec=15min\n\
         \n\
         [Install]\n\
         WantedBy=timers.target\n"
    )
}

/// The schedule an installed pair of unit files describes, read back from
/// them; `None` when they are not ddc's own shape.
pub fn read_spec(service: &str, timer: &str) -> Option<UnitSpec> {
    let exec = service.lines().find_map(|l| l.strip_prefix("ExecStart="))?;
    let args = words(exec, true)?;
    let mut it = args.iter();
    let exe = PathBuf::from(it.next()?);
    let (mut data_dir, mut threshold) = (None, None);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--data-dir" => data_dir = Some(PathBuf::from(it.next()?)),
            "--threshold" => threshold = Some(it.next()?.parse().ok()?),
            _ => {}
        }
    }
    let pinned = service
        .lines()
        .filter_map(|l| l.strip_prefix("Environment="))
        .map(|v| {
            let word = words(v, false)?.into_iter().next()?;
            let (name, value) = word.split_once('=')?;
            Some((name.to_owned(), PathBuf::from(value)))
        })
        .collect::<Option<Vec<_>>>()?;
    let interval = match timer.lines().find_map(|l| l.strip_prefix("OnCalendar="))? {
        "daily" => IntervalArg::Daily,
        "weekly" => IntervalArg::Weekly,
        _ => return None,
    };
    Some(UnitSpec { exe, interval, threshold, data_dir: data_dir?, pinned })
}

/// The crontab line that runs the same scan, for a machine without a user
/// systemd. ddc never edits a crontab.
pub fn cron_line(spec: &UnitSpec) -> String {
    let sh = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
    let when = match spec.interval {
        IntervalArg::Daily => "30 11 * * *",
        IntervalArg::Weekly => "30 11 * * 1",
    };
    let threshold = spec.threshold.map(|t| format!(" --threshold {t}")).unwrap_or_default();
    // `%` ends a crontab command line unless escaped.
    format!(
        "{when} {} scan --background --data-dir {}{threshold}",
        sh(&spec.exe.to_string_lossy()),
        sh(&spec.data_dir.to_string_lossy())
    )
    .replace('%', r"\%")
}

/// What one `systemctl --user` call gave.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub trait Systemctl {
    /// `systemctl --user <args>`; `Err` when it cannot run at all.
    fn user(&self, args: &[&str]) -> io::Result<Out>;
    /// Whether the user's manager lingers: `None` when it cannot be told.
    fn linger(&self) -> Option<bool>;
}

pub struct SystemSystemctl;

/// `systemctl` and `loginctl` live in `/usr/bin` or, on older layouts, `/bin`;
/// never looked up through `PATH`.
fn tool(name: &str) -> io::Result<PathBuf> {
    ["/usr/bin", "/bin"]
        .iter()
        .map(|d| Path::new(d).join(name))
        .find(|p| p.is_file())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{name} not found")))
}

impl Systemctl for SystemSystemctl {
    fn user(&self, args: &[&str]) -> io::Result<Out> {
        let out = Command::new(tool("systemctl")?).arg("--user").args(args).output()?;
        Ok(Out {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    fn linger(&self) -> Option<bool> {
        let uid = ddc_core::platform::procs::uid().to_string();
        let out = Command::new(tool("loginctl").ok()?)
            .args(["show-user", uid.as_str(), "--property=Linger", "--value"])
            .output()
            .ok()?;
        match String::from_utf8_lossy(&out.stdout).trim() {
            "yes" => Some(true),
            "no" => Some(false),
            _ => None,
        }
    }
}

/// The systemd user timer of the user whose home is `home`.
pub struct Systemd<'a> {
    pub home: PathBuf,
    pub sc: &'a dyn Systemctl,
}

impl Systemd<'_> {
    fn paths(&self) -> (PathBuf, PathBuf) {
        let dir = unit_dir(&self.home);
        (dir.join(SERVICE), dir.join(TIMER))
    }

    /// `systemctl --user <args>`, failing on a non-zero exit.
    fn run(&self, args: &[&str]) -> anyhow::Result<()> {
        let out = self.sc.user(args)?;
        anyhow::ensure!(out.code == 0, "systemctl --user {}: {}", args.join(" "), out.stderr.trim());
        Ok(())
    }

    /// `systemctl --user show` of `props`, as `name=value` pairs.
    fn show(&self, unit: &str, props: &str) -> Vec<(String, String)> {
        self.sc
            .user(&["show", unit, "--property", props])
            .ok()
            .filter(|o| o.code == 0)
            .map(|o| {
                o.stdout.lines().filter_map(|l| l.split_once('=')).map(|(k, v)| (k.to_owned(), v.to_owned())).collect()
            })
            .unwrap_or_default()
    }

    /// A user manager runs: `is-system-running` exits 0 or prints a state of
    /// one that runs.
    fn manager(&self) -> bool {
        self.sc.user(&["is-system-running"]).is_ok_and(|o| o.code == 0 || LIVE.contains(&o.stdout.trim()))
    }

    /// Disables the timer and the service, which removes the links `link` and
    /// `enable` made. On some systemd versions a link of the same name that
    /// points at a moved or deleted folder makes `disable` fail: ours are then
    /// linked again with `--force`, and disabled.
    fn disable_both(&self, service: &Path, timer: &Path) -> anyhow::Result<()> {
        let disable = || -> anyhow::Result<()> {
            self.run(&["disable", "--now", TIMER])?;
            self.run(&["disable", SERVICE])
        };
        if disable().is_ok() {
            return Ok(());
        }
        for path in [service, timer] {
            if path.exists() {
                let _ = self.sc.user(&["link", "--force", &path.to_string_lossy()]);
            }
        }
        disable().map_err(|e| e.context("the timer is still enabled; its unit files were kept"))
    }

    fn unavailable(&self, spec: &UnitSpec) -> Unavailable {
        Unavailable {
            code: "no-user-systemd",
            message: format!(
                "no systemd user manager runs for this user, so ddc cannot install a timer. {RUNTIME_DIR_HINT}; \
                 otherwise add this line with `crontab -e`:"
            ),
            instead: cron_line(spec),
        }
    }
}

impl Scheduler for Systemd<'_> {
    fn available(&self, spec: &ScheduleSpec) -> Result<(), Unavailable> {
        if self.manager() {
            return Ok(());
        }
        let unit = UnitSpec {
            exe: scheduled_program(&spec.exe),
            interval: spec.interval,
            threshold: spec.threshold,
            data_dir: spec.data_dir.clone(),
            pinned: Vec::new(),
        };
        Err(self.unavailable(&unit))
    }

    fn install(&self, spec: &ScheduleSpec) -> anyhow::Result<Installed> {
        refuse_cargo_target(&spec.exe)?;
        let unit = UnitSpec {
            exe: scheduled_program(&spec.exe),
            interval: spec.interval,
            threshold: spec.threshold,
            data_dir: spec.data_dir.clone(),
            pinned: spec.pinned.iter().map(|(n, v)| ((*n).to_owned(), v.clone())).collect(),
        };
        let text = service_text(&unit).ok_or_else(|| {
            anyhow::anyhow!("a path of the schedule holds a control character, which a unit file cannot carry")
        })?;
        std::fs::create_dir_all(&spec.log_dir)?;
        let (service, timer) = self.paths();
        // An earlier pair, ours or one linked from a folder that moved, goes
        // first; `--force` replaces a link of the same name that points
        // elsewhere.
        let _ = self.sc.user(&["disable", "--now", TIMER]);
        let _ = self.sc.user(&["disable", SERVICE]);
        ddc_core::store::write_atomic(&service, text.as_bytes())?;
        ddc_core::store::write_atomic(&timer, timer_text(unit.interval).as_bytes())?;
        self.run(&["link", "--force", &service.to_string_lossy()])?;
        self.run(&["daemon-reload"])?;
        self.run(&["enable", "--force", "--now", &timer.to_string_lossy()])?;
        // A changed `OnCalendar` takes effect only on a restart.
        self.run(&["restart", TIMER])?;
        Ok(Installed { path: timer, try_now: format!("systemctl --user start {SERVICE}") })
    }

    fn uninstall(&self) -> anyhow::Result<bool> {
        let (service, timer) = self.paths();
        let files = service.exists() || timer.exists();
        if !self.manager() {
            // Without the manager nothing can be disabled: removing the files
            // would leave an enabled timer that runs nothing.
            anyhow::ensure!(
                !files,
                "no systemd user manager answers, so the timer cannot be disabled; its unit files were kept. \
                 {RUNTIME_DIR_HINT}"
            );
            return Ok(false);
        }
        if !files {
            // No unit files of ours. A link left from a moved or deleted
            // folder, or a timer still loaded, is removed all the same.
            let known = |unit| {
                self.show(unit, "LoadState,UnitFileState")
                    .iter()
                    .any(|(k, v)| (k == "LoadState" && v == "loaded") || (k == "UnitFileState" && !v.is_empty()))
            };
            let left = known(TIMER) || known(SERVICE);
            if left {
                let _ = self.sc.user(&["disable", "--now", TIMER]);
                let _ = self.sc.user(&["disable", SERVICE]);
                self.run(&["daemon-reload"])?;
            }
            return Ok(left);
        }
        self.disable_both(&service, &timer)?;
        for path in [&service, &timer] {
            if path.exists() {
                #[allow(clippy::disallowed_methods)] // our own unit file
                std::fs::remove_file(path)?;
            }
        }
        self.run(&["daemon-reload"])?;
        Ok(true)
    }

    fn status(&self, current: Option<&Path>) -> Option<Status> {
        let (service_path, timer_path) = self.paths();
        let service = std::fs::read_to_string(&service_path).ok();
        let timer = std::fs::read_to_string(&timer_path).ok();
        if service.is_none() && timer.is_none() {
            // Nothing installed: no call at all.
            return None;
        }
        let spec = read_spec(service.as_deref().unwrap_or(""), timer.as_deref().unwrap_or(""));
        let mut lines = vec![format!("Units:   {}, {}", service_path.display(), timer_path.display())];
        match &spec {
            Some(spec) => {
                let current_text = service.as_deref().is_some_and(|t| service_text(spec).as_deref() == Some(t))
                    && timer.as_deref() == Some(timer_text(spec.interval).as_str());
                lines.push(match program_state(&spec.exe, current) {
                    ProgramState::Missing => format!("Binary:  {} (missing! reinstall)", spec.exe.display()),
                    ProgramState::Other => format!("Binary:  {} (not this ddc)", spec.exe.display()),
                    ProgramState::Current if current_text => format!("Binary:  {}", spec.exe.display()),
                    ProgramState::Current => format!(
                        "Binary:  {} (units written by another ddc version: run `ddc schedule install` again)",
                        spec.exe.display()
                    ),
                });
            }
            None => lines.push("Binary:  unreadable unit files: run `ddc schedule install` again".to_owned()),
        }
        let timer_show = self.show(TIMER, "UnitFileState,NextElapseUSecRealtime");
        let service_show = self.show(SERVICE, "Result,ExecMainStatus");
        let get = |pairs: &[(String, String)], k: &str| {
            pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).filter(|v| !v.is_empty())
        };
        let state = get(&timer_show, "UnitFileState");
        lines.push(format!(
            "Enabled: {}",
            match state.as_deref() {
                Some("enabled") => "yes",
                Some("linked") => "no, linked but not enabled: run `ddc schedule install` again",
                Some(_) => "no",
                None => "unknown (no answer from the user's systemd)",
            }
        ));
        lines.push(format!("Next:    {}", get(&timer_show, "NextElapseUSecRealtime").unwrap_or_else(|| "none".into())));
        lines.push(format!(
            "Result:  {}",
            match (get(&service_show, "Result"), get(&service_show, "ExecMainStatus")) {
                (Some(r), Some(code)) => format!("{r} (exit {code})"),
                (Some(r), None) => r,
                _ => "not run yet".into(),
            }
        ));
        if let Some(spec) = &spec {
            lines.push(format!("Data:    {}", spec.data_dir.display()));
            let pinned: Vec<String> = spec.pinned.iter().map(|(n, v)| format!("{n}={}", v.display())).collect();
            lines.push(format!("Pinned:  {}", if pinned.is_empty() { "none".to_owned() } else { pinned.join(", ") }));
        }
        lines.push(match self.sc.linger() {
            Some(true) => "Linger:  yes, the timer runs while you are logged out".to_owned(),
            Some(false) => "Linger:  no, the timer runs only while you are logged in; to change that: \
                            loginctl enable-linger $USER"
                .to_owned(),
            None => "Linger:  unknown".to_owned(),
        });
        let notifies = ddc_core::platform::CAPABILITIES.contains(&ddc_core::platform::Capability::Notifications);
        lines.push(format!(
            "Notify:  {}",
            if notifies { "yes" } else { "no, desktop notifications are not built for this platform yet" }
        ));
        Some(Status { lines, data_dir: spec.map(|s| s.data_dir) })
    }
}

/// A `systemctl` that only records: no test runs the real one.
#[cfg(test)]
pub(crate) mod fake {
    use std::cell::RefCell;
    use std::io;

    use super::{Out, Systemctl};

    /// `state` is what `is-system-running` prints (`None`: it cannot run);
    /// `fail` makes the call starting with it exit 1.
    pub(crate) struct FakeSystemctl {
        pub calls: RefCell<Vec<String>>,
        pub state: Option<&'static str>,
        pub fail: Option<&'static str>,
        /// Like `fail`, for the first matching call only.
        pub fail_once: std::cell::Cell<Option<&'static str>>,
        pub linger: Option<bool>,
        pub show: RefCell<Vec<(String, String)>>,
    }

    impl Default for FakeSystemctl {
        fn default() -> Self {
            Self {
                calls: RefCell::default(),
                state: Some("running"),
                fail: None,
                fail_once: std::cell::Cell::new(None),
                linger: Some(false),
                show: RefCell::default(),
            }
        }
    }

    impl Systemctl for FakeSystemctl {
        fn user(&self, args: &[&str]) -> io::Result<Out> {
            let line = args.join(" ");
            self.calls.borrow_mut().push(line.clone());
            if args.first() == Some(&"is-system-running") {
                return match self.state {
                    None => Err(io::Error::new(io::ErrorKind::NotFound, "systemctl not found")),
                    Some(s) => Ok(Out { code: i32::from(s != "running"), stdout: format!("{s}\n"), ..Out::default() }),
                };
            }
            if self.fail_once.get().is_some_and(|f| line.starts_with(f)) {
                self.fail_once.set(None);
                return Ok(Out { code: 1, stderr: "Failed".into(), ..Out::default() });
            }
            if self.fail.is_some_and(|f| line.starts_with(f)) {
                return Ok(Out { code: 1, stderr: "Failed".into(), ..Out::default() });
            }
            if args.first() == Some(&"show") {
                let props = args.get(3).copied().unwrap_or_default();
                let stdout = self
                    .show
                    .borrow()
                    .iter()
                    .filter(|(k, _)| props.split(',').any(|p| p == k))
                    .map(|(k, v)| format!("{k}={v}\n"))
                    .collect();
                return Ok(Out { code: 0, stdout, ..Out::default() });
            }
            Ok(Out::default())
        }

        fn linger(&self) -> Option<bool> {
            self.linger
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::fake::FakeSystemctl;
    use super::*;

    fn unit() -> UnitSpec {
        UnitSpec {
            exe: PathBuf::from("/home/me/.local/bin/deepdrivecleaner \"50%\" $HOME"),
            interval: IntervalArg::Daily,
            threshold: Some(10 << 30),
            data_dir: PathBuf::from("/home/me/ddc data"),
            pinned: vec![("DOCKER_CONFIG".into(), PathBuf::from("/home/me/.config/docker 100%"))],
        }
    }

    /// The service runs the scan in the background, at low priority, with the
    /// data folder pinned; `%`, `$`, quotes and spaces survive the unit syntax
    /// and read back as written.
    #[test]
    fn the_unit_texts_quote_every_word_and_read_back() {
        let spec = unit();
        let service = service_text(&spec).unwrap();
        assert!(service.contains("Type=oneshot\n") && service.contains("Nice=10\n"), "{service}");
        assert!(service.contains("IOSchedulingClass=idle\n"));
        assert!(
            service.contains(
                r#"ExecStart="/home/me/.local/bin/deepdrivecleaner \"50%%\" $$HOME" scan --background --data-dir "/home/me/ddc data" --threshold 10737418240"#
            ),
            "{service}"
        );
        assert!(service.contains(r#"Environment="DOCKER_CONFIG=/home/me/.config/docker 100%%""#), "{service}");
        let timer = timer_text(IntervalArg::Weekly);
        for line in
            ["OnCalendar=weekly\n", "Persistent=true\n", "RandomizedDelaySec=15min\n", "WantedBy=timers.target\n"]
        {
            assert!(timer.contains(line), "{timer}");
        }
        assert_eq!(read_spec(&service, &timer_text(IntervalArg::Daily)), Some(spec.clone()));
        let weekly = UnitSpec { interval: IntervalArg::Weekly, threshold: None, pinned: Vec::new(), ..spec };
        assert_eq!(read_spec(&service_text(&weekly).unwrap(), &timer), Some(weekly));
        let newline = UnitSpec { data_dir: PathBuf::from("/home/me/a\nExecStartPre=/bin/evil"), ..unit() };
        assert_eq!(service_text(&newline), None, "a newline would start a unit line of its own");
    }

    #[test]
    fn the_cron_line_runs_the_same_scan() {
        let line = cron_line(&UnitSpec { threshold: None, ..unit() });
        assert!(
            line.starts_with("30 11 * * * '/home/me/.local/bin/deepdrivecleaner \"50\\%\" $HOME' scan --background"),
            "{line}"
        );
        assert!(line.ends_with("--data-dir '/home/me/ddc data'"), "{line}");
    }

    fn spec(tmp: &Path) -> ScheduleSpec {
        let exe = tmp.join("bin/deepdrivecleaner");
        std::fs::create_dir_all(exe.parent().unwrap()).unwrap();
        std::fs::write(&exe, "binary").unwrap();
        ScheduleSpec {
            exe,
            interval: IntervalArg::Daily,
            threshold: None,
            log_dir: tmp.join("data/logs"),
            data_dir: tmp.join("data"),
            data_dir_is_default: false,
            pinned: vec![("CLAUDE_CODE_TMPDIR", tmp.join("scratch"))],
        }
    }

    /// Any state of a running manager counts, `degraded` too; `offline`
    /// (WSL or a container without systemd), `unknown` or no systemctl at
    /// all do not.
    #[test]
    fn only_a_manager_that_runs_is_available() {
        let tmp = tempfile::tempdir().unwrap();
        for (state, ok) in [
            (Some("running"), true),
            (Some("degraded"), true),
            (Some("starting"), true),
            (Some("offline"), false),
            (Some("unknown"), false),
            (Some(""), false),
            (None, false),
        ] {
            let sc = FakeSystemctl { state, ..FakeSystemctl::default() };
            let sd = Systemd { home: tmp.path().to_path_buf(), sc: &sc };
            assert_eq!(sd.available(&spec(tmp.path())).is_ok(), ok, "{state:?}");
        }
        let sc = FakeSystemctl { state: Some("offline"), ..FakeSystemctl::default() };
        let sd = Systemd { home: tmp.path().to_path_buf(), sc: &sc };
        let refused = sd.available(&spec(tmp.path())).unwrap_err();
        assert_eq!(refused.code, "no-user-systemd");
        assert!(refused.message.contains("XDG_RUNTIME_DIR") && refused.message.contains("crontab -e"));
        assert!(refused.instead.starts_with("30 11 * * * "), "{}", refused.instead);
        assert!(!unit_dir(tmp.path()).exists(), "nothing written");
    }

    /// Install writes both files, links the service, enables and restarts the
    /// timer; again, it first disables the earlier pair. Uninstall disables
    /// both and removes the files; again, it is a no-op.
    #[test]
    fn install_and_uninstall_are_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let sc = FakeSystemctl::default();
        let sd = Systemd { home: tmp.path().to_path_buf(), sc: &sc };
        let (service, timer) = sd.paths();

        let installed = sd.install(&spec(tmp.path())).unwrap();
        assert_eq!(installed.path, timer);
        assert_eq!(installed.try_now, "systemctl --user start deepdrivecleaner-scan.service");
        let text = std::fs::read_to_string(&service).unwrap();
        assert!(text.contains(&format!("--data-dir \"{}\"", tmp.path().join("data").display())), "{text}");
        assert!(text.contains("Environment=\"CLAUDE_CODE_TMPDIR="), "{text}");
        assert_eq!(
            sc.calls.take(),
            vec![
                "disable --now deepdrivecleaner-scan.timer".to_owned(),
                "disable deepdrivecleaner-scan.service".to_owned(),
                format!("link --force {}", service.display()),
                "daemon-reload".to_owned(),
                format!("enable --force --now {}", timer.display()),
                "restart deepdrivecleaner-scan.timer".to_owned(),
            ]
        );

        sd.install(&ScheduleSpec { interval: IntervalArg::Weekly, ..spec(tmp.path()) }).unwrap();
        let calls = sc.calls.take();
        assert_eq!(&calls[..2], ["disable --now deepdrivecleaner-scan.timer", "disable deepdrivecleaner-scan.service"]);
        assert!(std::fs::read_to_string(&timer).unwrap().contains("OnCalendar=weekly\n"));

        assert!(sd.uninstall().unwrap());
        assert!(!service.exists() && !timer.exists());
        let calls = sc.calls.take();
        assert_eq!(calls[0], "is-system-running", "{calls:?}");
        for call in
            ["disable --now deepdrivecleaner-scan.timer", "disable deepdrivecleaner-scan.service", "daemon-reload"]
        {
            assert!(calls.iter().any(|c| c == call), "{call}: {calls:?}");
        }
        assert!(!sd.uninstall().unwrap(), "a second uninstall is a no-op");
    }

    /// A timer that cannot be disabled keeps its unit files, so `status`
    /// still shows what runs.
    #[test]
    fn a_failed_disable_keeps_the_unit_files() {
        let tmp = tempfile::tempdir().unwrap();
        let sc = FakeSystemctl::default();
        let sd = Systemd { home: tmp.path().to_path_buf(), sc: &sc };
        sd.install(&spec(tmp.path())).unwrap();
        let failing = FakeSystemctl { fail: Some("disable --now"), ..FakeSystemctl::default() };
        let sd = Systemd { home: tmp.path().to_path_buf(), sc: &failing };
        assert!(sd.uninstall().is_err());
        assert!(sd.paths().0.exists() && sd.paths().1.exists());
    }

    /// Nothing installed: status makes no call. Installed: it names the
    /// units, the binary, the timer's state, the data folder, the pins and
    /// the linger hint, and reports the data folder for the last scan.
    #[test]
    fn status_reads_the_units_and_calls_nothing_when_there_are_none() {
        let tmp = tempfile::tempdir().unwrap();
        let sc = FakeSystemctl::default();
        let sd = Systemd { home: tmp.path().to_path_buf(), sc: &sc };
        assert!(sd.status(None).is_none());
        assert!(sc.calls.borrow().is_empty(), "no call without unit files");

        let s = spec(tmp.path());
        sd.install(&s).unwrap();
        sc.show.borrow_mut().extend([
            ("UnitFileState".to_owned(), "enabled".to_owned()),
            ("NextElapseUSecRealtime".to_owned(), "Tue 2026-11-03 00:07:12 UTC".to_owned()),
            ("Result".to_owned(), "success".to_owned()),
            ("ExecMainStatus".to_owned(), "0".to_owned()),
        ]);
        let status = sd.status(Some(&s.exe)).unwrap();
        assert_eq!(status.data_dir, Some(tmp.path().join("data")));
        let text = status.lines.join("\n");
        assert!(text.contains(&format!("Binary:  {}\n", s.exe.display())), "{text}");
        for part in [
            "Enabled: yes",
            "Next:    Tue 2026-11-03 00:07:12 UTC",
            "Result:  success (exit 0)",
            "Pinned:  CLAUDE_CODE_TMPDIR=",
            "loginctl enable-linger $USER",
        ] {
            assert!(text.contains(part), "{part}: {text}");
        }

        // A unit an older ddc wrote says so.
        let (service, _) = sd.paths();
        let old = std::fs::read_to_string(&service).unwrap().replace("Nice=10\n", "");
        std::fs::write(&service, old).unwrap();
        let text = sd.status(Some(&s.exe)).unwrap().lines.join("\n");
        assert!(text.contains("run `ddc schedule install` again"), "{text}");
    }

    /// Without a manager the unit files stay, and the message says how to
    /// reach it; with no files, nothing is claimed.
    #[test]
    fn uninstall_without_a_manager_keeps_the_files_and_says_why() {
        let tmp = tempfile::tempdir().unwrap();
        let sc = FakeSystemctl::default();
        let sd = Systemd { home: tmp.path().to_path_buf(), sc: &sc };
        sd.install(&spec(tmp.path())).unwrap();
        let away = FakeSystemctl { state: None, ..FakeSystemctl::default() };
        let sd = Systemd { home: tmp.path().to_path_buf(), sc: &away };
        let err = sd.uninstall().unwrap_err().to_string();
        assert!(err.contains("XDG_RUNTIME_DIR") && err.contains("were kept"), "{err}");
        assert!(sd.paths().0.exists() && sd.paths().1.exists());

        let empty = tempfile::tempdir().unwrap();
        let sd = Systemd { home: empty.path().to_path_buf(), sc: &away };
        assert!(!sd.uninstall().unwrap());
    }

    /// No unit files of ours, but a link systemd still knows (from a folder
    /// that moved): uninstall removes it and says it removed something.
    #[test]
    fn uninstall_cleans_a_link_left_without_files() {
        let tmp = tempfile::tempdir().unwrap();
        let sc = FakeSystemctl::default();
        sc.show.borrow_mut().push(("UnitFileState".to_owned(), "linked".to_owned()));
        let sd = Systemd { home: tmp.path().to_path_buf(), sc: &sc };
        assert!(sd.uninstall().unwrap());
        let calls = sc.calls.take();
        assert!(calls.iter().any(|c| c == "disable --now deepdrivecleaner-scan.timer"), "{calls:?}");

        let clean = FakeSystemctl::default();
        let sd = Systemd { home: tmp.path().to_path_buf(), sc: &clean };
        assert!(!sd.uninstall().unwrap(), "nothing known: nothing claimed");
    }

    /// A `disable` that fails once (a link of the same name points at a moved
    /// folder) is retried after our files are linked again with `--force`.
    #[test]
    fn a_failed_disable_relinks_ours_and_tries_again() {
        let tmp = tempfile::tempdir().unwrap();
        let sc = FakeSystemctl::default();
        let sd = Systemd { home: tmp.path().to_path_buf(), sc: &sc };
        sd.install(&spec(tmp.path())).unwrap();
        let once = FakeSystemctl { fail_once: std::cell::Cell::new(Some("disable --now")), ..FakeSystemctl::default() };
        let sd = Systemd { home: tmp.path().to_path_buf(), sc: &once };
        assert!(sd.uninstall().unwrap());
        let calls = once.calls.take();
        assert!(calls.iter().any(|c| c.starts_with("link --force ")), "{calls:?}");
        assert!(!sd.paths().0.exists());
    }

    #[test]
    fn a_cargo_build_is_never_scheduled() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        let exe = tmp.path().join("target/debug/deepdrivecleaner");
        let sc = FakeSystemctl::default();
        let sd = Systemd { home: tmp.path().to_path_buf(), sc: &sc };
        assert!(sd.install(&ScheduleSpec { exe, ..spec(tmp.path()) }).is_err());
        assert!(!unit_dir(tmp.path()).exists());
    }
}
