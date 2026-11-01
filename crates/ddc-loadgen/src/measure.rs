//! Runs the CLI once and measures it: wall time, its own user and system
//! time and peak memory from `wait4`, and the peak thread count sampled
//! every 10 ms. Output goes to files, never pipes: `scan --json` at M is
//! megabytes, and a full pipe would block the child.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use ddc_core::platform::procs;
use ddc_core::platform::usage::{self, Usage};

use crate::isolate::ClearEnv as _;

const POLL: Duration = Duration::from_millis(10);

#[derive(Debug, Clone)]
pub struct Measured {
    pub wall: Duration,
    pub usage: Usage,
    pub threads_max: u32,
    pub status: ExitStatus,
    pub stdout: PathBuf,
    pub stderr: PathBuf,
}

/// Runs `cli args` in `home` with a cleared environment (`HOME`, `PATH`
/// empty, `NO_COLOR`), its output in `<out>.stdout` and `<out>.stderr`.
/// A relative `cli` is resolved against this process's directory, not
/// `home`. The caller
/// has checked the run ([`crate::isolate::decide`]).
// The child is reaped by `wait4`, never by `Child::wait`, which would reap it
// first and lose its usage.
#[allow(clippy::zombie_processes)]
pub fn measure(cli: &Path, home: &Path, args: &[OsString], out: &Path) -> io::Result<Measured> {
    let stdout = out.with_extension("stdout");
    let stderr = out.with_extension("stderr");
    let cli = cli.canonicalize()?;
    let started = Instant::now();
    let child = Command::new(cli)
        .clear_env()
        .home_env(home)
        .env("PATH", "")
        .env("NO_COLOR", "1")
        .current_dir(home)
        .args(args)
        .stdin(Stdio::null())
        .stdout(File::create(&stdout)?)
        .stderr(File::create(&stderr)?)
        .spawn()?;
    let pid = child.id();
    let mut threads_max = 0;
    loop {
        if let Some(n) = i32::try_from(pid).ok().and_then(procs::thread_count) {
            threads_max = threads_max.max(n);
        }
        if let Some((status, usage)) = usage::wait4(pid, true)? {
            return Ok(Measured { wall: started.elapsed(), usage, threads_max, status, stdout, stderr });
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `make load` passes `target/release/deepdrivecleaner`: a path relative
    /// to the repository, while the child runs in its tree's home.
    #[test]
    fn a_relative_cli_runs_in_another_directory() {
        // The measured child is waited for with wait4, not ported off macOS yet.
        if crate::unsupported("a_relative_cli_runs_in_another_directory") {
            return;
        }
        let here = std::env::current_dir().unwrap();
        let mut relative = PathBuf::new();
        here.components().skip(1).for_each(|_| relative.push(".."));
        relative.push("bin/echo");
        assert!(relative.is_relative());
        let home = tempfile::tempdir().unwrap();
        let m = measure(&relative, home.path(), &[OsString::from("hi")], &home.path().join("echo")).unwrap();
        assert!(m.status.success(), "{:?}", m.status);
        assert_eq!(std::fs::read_to_string(&m.stdout).unwrap(), "hi\n");
    }
}
