//! Real ports: tools on disk, the macOS Trash, the process table.

use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{CmdOutput, CommandRunner, ProcessTable, TrashError, TrashPlace, TrashTarget, Trasher};
use crate::env::Env;
use crate::platform::procs::{self, OpenVnode, ProcInfo, ProcStart};
use crate::platform::{meta, process, signals, trash};

/// Finds tools in `Env::tool_dirs()` (plus `/usr/bin` for system tools).
pub struct SystemCommands {
    dirs: Vec<PathBuf>,
}

impl SystemCommands {
    pub fn new(env: &Env) -> Self {
        let mut dirs = env.tool_dirs();
        dirs.push(PathBuf::from("/usr/bin"));
        // Only absolute folders: on Windows `/usr/bin` names a folder at the
        // current drive's root, which any user may create.
        dirs.retain(|d| d.is_absolute());
        Self { dirs }
    }
}

impl CommandRunner for SystemCommands {
    /// In each folder, the program's names in the platform's order
    /// (`process::program_names`: on Windows `.exe`, `.cmd`, `.bat`).
    fn find(&self, program: &str) -> Option<PathBuf> {
        let names = process::program_names(program);
        self.dirs.iter().flat_map(|d| names.iter().map(move |n| d.join(n))).find(|p| executable(p))
    }

    /// The command gets its own process group: a timeout or a forwarded
    /// SIGINT/SIGTERM reaches everything it started, and a grandchild cannot
    /// keep the pipes open after the command is gone.
    fn run(&self, exe: &Path, args: &[String], env: &[(&str, &str)], timeout: Duration) -> io::Result<CmdOutput> {
        let mut cmd = Command::new(exe);
        cmd.args(args).envs(env.iter().copied());
        spawn(exe, cmd, timeout, false)
    }

    /// The names are read from this process's environment as the command
    /// starts, and each one is removed by name: no `env_clear`, so `PATH`,
    /// `HOME` and the rest stay.
    fn run_cleared(
        &self,
        exe: &Path,
        args: &[String],
        cleared: &str,
        env: &[(&str, &str)],
        timeout: Duration,
    ) -> io::Result<CmdOutput> {
        let mut cmd = Command::new(exe);
        cmd.args(args);
        for name in inherited(cleared) {
            cmd.env_remove(name);
        }
        cmd.envs(env.iter().copied());
        spawn(exe, cmd, timeout, true)
    }
}

/// The names of the variables of this process's environment that start
/// with `prefix`; none for an empty prefix.
fn inherited(prefix: &str) -> Vec<std::ffi::OsString> {
    if prefix.is_empty() {
        return Vec::new();
    }
    std::env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| name.to_str().is_some_and(|n| n.starts_with(prefix)))
        .collect()
}

/// Runs `cmd`, stdin closed and its output piped, in its own process group.
/// `lossy`: output that is not UTF-8 is kept with replacement characters,
/// so a git probe never reads a file name it cannot decode as no output at
/// all; otherwise such output is empty, as every other tool's always was
/// (a readiness probe must not take bytes it cannot read for an answer).
fn spawn(exe: &Path, mut cmd: Command, timeout: Duration, lossy: bool) -> io::Result<CmdOutput> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = process::spawn_grouped(&mut cmd)?;
    // The group id is the child's pid; a signal that came before this is forwarded now.
    let group = i32::try_from(child.id()).unwrap_or(0);
    signals::set_child_group(group);
    // Drain both pipes on threads so a chatty command cannot block on a full pipe.
    let mut out = child.stdout.take().expect("piped");
    let mut err = child.stderr.take().expect("piped");
    let decode = move |bytes: Vec<u8>| match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) if lossy => String::from_utf8_lossy(e.as_bytes()).into_owned(),
        Err(_) => String::new(),
    };
    let out_t = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = out.read_to_end(&mut bytes);
        decode(bytes)
    });
    let err_t = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = err.read_to_end(&mut bytes);
        decode(bytes)
    });
    let started = Instant::now();
    let waited = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) if started.elapsed() > timeout => {
                signals::kill_group(group, signals::SIGKILL);
                let _ = child.wait();
                break Err(io::Error::new(io::ErrorKind::TimedOut, format!("{} timed out", exe.display())));
            }
            Ok(None) => std::thread::park_timeout(Duration::from_millis(50)),
            Err(e) => {
                // Its pipes are drained below: a command left running would hold them open.
                signals::kill_group(group, signals::SIGKILL);
                break Err(e);
            }
        }
    };
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();
    // Cleared only once the pipes are drained: a process the command left
    // behind in its group can hold them open, and a signal meanwhile must
    // still reach it. The group id is not reused while a member lives.
    signals::clear_child_group(group);
    let status = waited?;
    Ok(CmdOutput { success: status.success(), code: status.code(), stdout, stderr })
}

/// Whether `path` is a file someone may execute (symlinks followed).
pub(super) fn executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && meta::is_executable(path, &m))
}

/// The system Trash (`platform::trash`): on macOS through `NSFileManager`
/// (no AppleScript, no Automation prompt); on Linux ddc's own move into a
/// freedesktop trash folder on the item's own mount (safety ADR §5,
/// amendment of 2026-11-01).
pub struct SystemTrash;

impl Trasher for SystemTrash {
    fn trash(&self, target: &TrashTarget<'_>) -> Result<Option<TrashPlace>, TrashError> {
        trash::trash_item(target.parent, target.name, target.path, target.id)
    }

    fn would_trash(&self, target: &TrashTarget<'_>) -> Result<(), TrashError> {
        trash::would_trash(target.parent, target.name, target.path)
    }
}

pub struct SystemProcesses;

impl ProcessTable for SystemProcesses {
    fn processes(&self) -> Vec<ProcInfo> {
        procs::current_user_processes()
    }
    fn open_files(&self, pid: i32) -> Vec<PathBuf> {
        procs::open_files(pid)
    }
    fn process_start(&self, pid: i32) -> Option<ProcStart> {
        procs::process_start(pid)
    }
    fn open_vnodes(&self, pid: i32) -> Vec<OpenVnode> {
        procs::open_vnodes(pid)
    }
    fn pids(&self) -> Vec<i32> {
        procs::current_user_processes_unopened().iter().map(|p| p.pid).collect()
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::exec::NATIVE_TIMEOUT;
    use crate::platform::usage::thread_cpu_time;

    /// Commands running at once each publish their process group, so a
    /// forwarded signal reaches every one of them (a scan's parallel git
    /// probes), and each forgets its own when it ends.
    #[test]
    fn parallel_commands_each_publish_their_group() {
        let dir = tempfile::tempdir().unwrap();
        let env =
            Env::with_paths(dir.path(), &dir.path().join("data"), &dir.path().join("logs"), SystemTime::now()).unwrap();
        let commands = SystemCommands::new(&env);
        let (sleep, args) = crate::platform::testing::sleep_command(1);
        let most = std::thread::scope(|s| {
            let runs: Vec<_> = (0..3).map(|_| s.spawn(|| commands.run(&sleep, &args, &[], NATIVE_TIMEOUT))).collect();
            let mut most = 0;
            let started = Instant::now();
            while started.elapsed() < Duration::from_millis(800) {
                most = most.max(crate::platform::signals::child_groups().len());
                std::thread::sleep(Duration::from_millis(20));
            }
            for run in runs {
                assert!(run.join().unwrap().unwrap().success);
            }
            most
        });
        assert!(most >= 3, "{most} groups published at once");
    }

    /// Output that is not UTF-8 is empty for a plain run, as before, and
    /// kept with replacement characters for a probe.
    #[test]
    fn only_a_probe_keeps_output_that_is_not_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let env =
            Env::with_paths(dir.path(), &dir.path().join("data"), &dir.path().join("logs"), SystemTime::now()).unwrap();
        let commands = SystemCommands::new(&env);
        let file = dir.path().join("bytes");
        std::fs::write(&file, b"a\xffb").unwrap();
        let (print, args) = crate::platform::testing::print_file_command(&file);
        let plain = commands.run(&print, &args, &[], NATIVE_TIMEOUT).unwrap();
        assert_eq!(plain.stdout, "");
        let probe = commands.run_cleared(&print, &args, "GIT_", &[], NATIVE_TIMEOUT).unwrap();
        assert_eq!(probe.stdout, "a\u{fffd}b");
    }

    /// The thread waiting for a native command sleeps between its looks: its
    /// own CPU over a 2 s command is under 1% of the wait.
    #[test]
    fn waiting_for_a_tool_does_not_spin() {
        let dir = tempfile::tempdir().unwrap();
        let env =
            Env::with_paths(dir.path(), &dir.path().join("data"), &dir.path().join("logs"), SystemTime::now()).unwrap();
        let (cpu, at) = (thread_cpu_time().unwrap(), Instant::now());
        let (sleep, args) = crate::platform::testing::sleep_command(2);
        let out = SystemCommands::new(&env).run(&sleep, &args, &[], NATIVE_TIMEOUT).unwrap();
        let (cpu, wall) = (thread_cpu_time().unwrap() - cpu, at.elapsed());
        assert!(out.success);
        assert!(wall >= Duration::from_millis(1500), "{wall:?}");
        let share = cpu.as_secs_f64() / wall.as_secs_f64();
        assert!(share < 0.01, "{:.2}% CPU", share * 100.0);
    }
}
