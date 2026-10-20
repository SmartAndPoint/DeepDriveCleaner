//! The warning `ddc` prints when it starts in a terminal (story "Distribution:
//! the 0.2.0 cask with the app"): every command but `--version` gets it on a
//! terminal, and nothing that reads its output (the app, launchd, a pipe, a
//! file) ever sees it.

use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_deepdrivecleaner");
const WARNING: &str = "ddc: warning: DeepDriveCleaner deletes files and can remove almost anything it is told to.\n              \
     Use it at your own risk: https://github.com/SmartAndPoint/DeepDriveCleaner/blob/main/DISCLAIMER.md\n";

struct Fx {
    tmp: tempfile::TempDir,
    home: PathBuf,
    data: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let (home, data) = (root.join("home"), root.join("data"));
        for (rel, content) in [
            ("p/package.json", &b"{}"[..]),
            ("p/package-lock.json", b"{}"),
            ("p/node_modules/x/index.js", &[1u8; 30_000]),
            (".npm/_cacache/content/blob", &[2u8; 90_000]),
        ] {
            let p = home.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        }
        Self { tmp, home, data }
    }

    fn env(&self, color: bool) -> Vec<(&'static str, &Path)> {
        let mut env =
            vec![("HOME", self.home.as_path()), ("DDC_DATA_DIR", self.data.as_path()), ("PATH", Path::new(""))];
        if !color {
            env.push(("NO_COLOR", Path::new("1")));
        }
        env
    }

    /// `ddc args` with stderr on a pseudo-terminal and stdout in a file, as
    /// `ddc scan --json > out.json` typed in a terminal: what the terminal
    /// showed (line ends as `\n`), and stdout.
    fn in_terminal(&self, args: &[&str], color: bool) -> (String, Vec<u8>) {
        let stdout = self.tmp.path().join("stdout");
        let mut child = Command::new("/usr/bin/script")
            .env_clear()
            .envs(self.env(color))
            .args(["-q", "/dev/null", "/bin/sh", "-c", r#"out=$1; shift; exec "$@" >"$out""#, "sh"])
            .arg(&stdout)
            .arg(BIN)
            .args(args)
            // Open while ddc runs: end of file would reach it as end of input.
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut shown = child.stdout.take().unwrap();
        let reader = std::thread::spawn(move || {
            let mut out = Vec::new();
            shown.read_to_end(&mut out).unwrap();
            out
        });
        let started = Instant::now();
        while child.try_wait().unwrap().is_none() {
            assert!(started.elapsed() < Duration::from_secs(60), "ddc {args:?} did not finish");
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(child.stdin.take());
        let shown = String::from_utf8(reader.join().unwrap()).unwrap().replace("\r\n", "\n");
        (shown, fs::read(&stdout).unwrap())
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.env_clear().envs(self.env(false)).args(args).stdin(Stdio::null());
        cmd
    }

    fn last_scan(&self) -> Vec<u8> {
        fs::read(self.data.join("last-scan.json")).unwrap()
    }
}

/// A report without what changes from one run to the next.
fn stable(json: &[u8]) -> serde_json::Value {
    let mut v: serde_json::Value = serde_json::from_slice(json).unwrap();
    let report = v.as_object_mut().unwrap();
    report.remove("generated_at");
    report.remove("duration_ms");
    for volume in report["volumes"].as_array_mut().unwrap() {
        volume.as_object_mut().unwrap().remove("free_bytes");
    }
    v
}

#[test]
fn every_command_but_version_starts_with_the_warning_in_a_terminal() {
    let fx = Fx::new();
    for args in [
        &[][..],
        &["--help"],
        &["-h"],
        &["help"],
        &["bogus"],
        &["scan", "--help"],
        &["clean", "--help"],
        &["schedule", "--help"],
        &["schedule", "status", "--help"],
        &["notify", "--help"],
        &["rules"],
        &["scan"],
        &["clean", "--dry-run"],
    ] {
        let (shown, _) = fx.in_terminal(args, false);
        assert!(shown.starts_with(WARNING), "ddc {args:?} showed:\n{shown}");
        assert_eq!(shown.matches("DISCLAIMER.md").count(), 1, "once: ddc {args:?} showed:\n{shown}");
    }
}

#[test]
fn version_has_no_warning() {
    let fx = Fx::new();
    for flag in ["--version", "-V"] {
        let (shown, stdout) = fx.in_terminal(&[flag], false);
        assert_eq!(shown, "");
        assert_eq!(String::from_utf8(stdout).unwrap(), format!("deepdrivecleaner {}\n", env!("CARGO_PKG_VERSION")));
    }
}

#[test]
fn the_label_is_colored_unless_no_color_is_set() {
    let fx = Fx::new();
    let (shown, _) = fx.in_terminal(&["rules"], true);
    assert!(shown.starts_with(&WARNING.replacen("warning:", "\x1b[1;33mwarning:\x1b[0m", 1)), "{shown:?}");
    let (shown, _) = fx.in_terminal(&["rules"], false);
    assert!(!shown.contains('\x1b'), "{shown:?}");
}

#[test]
fn nothing_that_reads_the_output_sees_the_warning_and_json_is_unchanged() {
    let fx = Fx::new();

    // In a terminal, with stdout redirected: stdout is the report and nothing else.
    let (shown, in_terminal) = fx.in_terminal(&["scan", "--json"], false);
    assert!(shown.starts_with(WARNING), "{shown}");
    assert_eq!(in_terminal, [fx.last_scan(), b"\n".to_vec()].concat(), "stdout is the saved report");

    // As the app runs it: stdout and stderr are pipes.
    let out = fx.command(&["scan", "--json"]).output().unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stderr), "", "no warning on a pipe");
    assert_eq!(out.stdout, [fx.last_scan(), b"\n".to_vec()].concat());
    assert_eq!(stable(&out.stdout), stable(&in_terminal), "the same report either way");

    // As launchd runs it: stderr is a file.
    let err = fx.tmp.path().join("err.log");
    let status = fx
        .command(&["rules", "--json"])
        .stdout(Stdio::null())
        .stderr(fs::File::create(&err).unwrap())
        .status()
        .unwrap();
    assert!(status.success());
    assert_eq!(fs::read_to_string(&err).unwrap(), "", "no warning in a log file");

    // A usage error on a pipe: clap's message alone.
    let out = fx.command(&["bogus"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).starts_with("error: "), "{}", String::from_utf8_lossy(&out.stderr));
}
