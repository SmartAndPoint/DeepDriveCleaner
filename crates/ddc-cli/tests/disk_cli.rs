//! `ddc disk` end to end (story "Whole disk: what lies outside the home
//! folder"): a temporary HOME and data directory, the system's folders and
//! tools under `--system-root`, and an empty `PATH`. The fake `diskutil`
//! fails, so the volumes come from `statfs`; nothing real is run.

use std::fs;
use std::io::Read as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::ExitStatusExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use ddc_core::disk::{DiskReport, HomeSource};
use ddc_loadgen::isolate::ClearEnv as _;

struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let fx = Self { root: tmp.path().canonicalize().unwrap(), _tmp: tmp };
        fx.write("sys/System/Volumes/Data/Applications/Tool.app/tool", 40_000);
        fx.write("sys/System/Volumes/Data/opt/homebrew/Cellar/jq/jq", 9000);
        fx.write("home/notes.txt", 5000);
        fx.tool("usr/sbin/diskutil", "#!/bin/sh\nexit 1\n");
        fx
    }

    fn write(&self, rel: &str, len: usize) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![7u8; len]).unwrap();
    }

    fn tool(&self, rel: &str, script: &str) {
        let path = self.root.join("sys").join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, script).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn ddc(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_deepdrivecleaner"));
        cmd.clear_env()
            .env("HOME", self.root.join("home"))
            .env("PATH", "")
            .env("NO_COLOR", "1")
            .arg("disk")
            .args(args)
            .arg("--data-dir")
            .arg(self.root.join("data"))
            .arg("--system-root")
            .arg(self.root.join("sys"))
            .stdin(Stdio::null());
        cmd
    }
}

fn report(stdout: &[u8]) -> DiskReport {
    serde_json::from_slice(stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(stdout)))
}

#[test]
fn disk_json_reports_the_areas_and_saves_the_same_report() {
    let fx = Fx::new();
    let out = fx.ddc(&["--json"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let r = report(&out.stdout);
    assert_eq!(r.schema_version, ddc_core::disk::DISK_SCHEMA_VERSION);
    assert!(!r.cancelled);
    let ids: Vec<&str> = r.areas.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, ["applications", "opt", "homebrew"]);
    assert!(r.areas.iter().all(|a| a.bytes > 0 || a.id == "opt"));
    assert_eq!(r.home.source, HomeSource::Walk);
    assert!(r.home.bytes >= 5000);
    assert!(r.simulators.is_none(), "no xcode-select under the system root");
    assert_eq!(r.snapshots.count, None);
    let saved = fs::read(fx.root.join("data/last-disk.json")).unwrap();
    assert_eq!(report(&saved), r);
}

#[test]
fn disk_prints_a_table_with_the_home_row_first() {
    let fx = Fx::new();
    let out = fx.ddc(&[]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    let rows: Vec<&str> = text.lines().filter(|l| l.starts_with("  ")).map(str::trim_start).collect();
    assert!(rows[0].starts_with("Home folder ~"), "{text}");
    assert!(rows[1].starts_with("Applications"), "{text}");
    assert!(rows.iter().any(|r| r.starts_with("Homebrew")), "{text}");
    assert!(rows.iter().any(|r| r.starts_with("Not counted")), "{text}");
}

#[test]
fn disk_with_progress_json_emits_disk_walk_ticks_ending_at_the_report_totals() {
    let fx = Fx::new();
    let out = fx.ddc(&["--json", "--progress", "json"]).output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let r = report(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let events: Vec<serde_json::Value> = stderr
        .lines()
        .filter(|l| l.starts_with('{'))
        .map(|l| {
            let event: ddc_core::progress::ProgressLine = serde_json::from_str(l).unwrap();
            serde_json::to_value(&event).unwrap()
        })
        .collect();
    assert!(!events.is_empty(), "{stderr}");
    assert!(events.iter().all(|e| e["event"] == "walk" && e["step"] == "disk"), "{events:?}");
    let last = events.last().unwrap();
    assert_eq!(
        (&last["files"], &last["dirs"], &last["bytes"]),
        (&serde_json::json!(r.walk.files), &serde_json::json!(r.walk.dirs), &serde_json::json!(r.walk.bytes))
    );
}

fn wait_for_pid(file: &Path, timeout: Duration) -> i32 {
    let started = Instant::now();
    loop {
        if let Some(pid) = fs::read_to_string(file).ok().filter(|s| s.ends_with('\n')) {
            return pid.trim().parse().unwrap_or_else(|_| panic!("not a pid: {pid:?}"));
        }
        assert!(started.elapsed() < timeout, "{file:?} never appeared");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wait_within(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

fn alive(pid: i32) -> bool {
    Command::new("/bin/kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().unwrap().success()
}

#[test]
fn sigterm_while_diskutil_runs_ends_it_and_still_saves_a_partial_report() {
    let fx = Fx::new();
    let pid_file = fx.root.join("diskutil.pid");
    fx.tool("usr/sbin/diskutil", &format!("#!/bin/sh\necho $$ > '{}'\nexec /bin/sleep 30\n", pid_file.display()));
    let mut child = fx.ddc(&["--json"]).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        stdout.read_to_end(&mut buf).unwrap();
        buf
    });
    let tool = wait_for_pid(&pid_file, Duration::from_secs(60));

    Command::new("/bin/kill").args(["-TERM", &child.id().to_string()]).status().unwrap();
    let status = wait_within(&mut child, Duration::from_secs(5)).expect("ddc ends within seconds");

    assert_eq!(status.signal(), Some(libc::SIGTERM), "ddc ends by the signal");
    assert!(!alive(tool), "diskutil's group got the signal");
    let r = report(&reader.join().unwrap());
    assert!(r.cancelled);
    assert!(!r.areas.is_empty());
    assert!(r.areas.iter().all(|a| a.partial), "{:?}", r.areas);
    assert!(r.home.partial);
    assert_eq!(report(&fs::read(fx.root.join("data/last-disk.json")).unwrap()), r);
}
