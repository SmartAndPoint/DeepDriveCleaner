//! Real ports: tools on disk, the macOS Trash, the process table.

use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{CmdOutput, CommandRunner, ProcessTable, Trasher};
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
        Self { dirs }
    }
}

impl CommandRunner for SystemCommands {
    fn find(&self, program: &str) -> Option<PathBuf> {
        self.dirs.iter().map(|d| d.join(program)).find(|p| executable(p))
    }

    /// The command gets its own process group: a timeout or a forwarded
    /// SIGINT/SIGTERM reaches everything it started, and a grandchild cannot
    /// keep the pipes open after the command is gone.
    fn run(&self, exe: &Path, args: &[String], env: &[(&str, &str)], timeout: Duration) -> io::Result<CmdOutput> {
        let mut cmd = Command::new(exe);
        cmd.args(args).envs(env.iter().copied());
        spawn(exe, cmd, timeout)
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
        spawn(exe, cmd, timeout)
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
fn spawn(exe: &Path, mut cmd: Command, timeout: Duration) -> io::Result<CmdOutput> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = process::own_process_group(&mut cmd).spawn()?;
    // The group id is the child's pid; a signal that came before this is forwarded now.
    let group = i32::try_from(child.id()).unwrap_or(0);
    signals::set_child_group(group);
    // Drain both pipes on threads so a chatty command cannot block on a full pipe.
    let mut out = child.stdout.take().expect("piped");
    let mut err = child.stderr.take().expect("piped");
    // Lossy: output that is not UTF-8 (a file name in `git status`) must
    // never read as no output at all.
    let out_t = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = out.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    });
    let err_t = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = err.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
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
    signals::clear_child_group();
    let status = waited?;
    Ok(CmdOutput { success: status.success(), code: status.code(), stdout, stderr })
}

/// Whether `path` is a file someone may execute (symlinks followed).
pub(super) fn executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && meta::is_executable(&m))
}

/// The system Trash (`platform::trash`): on macOS through `NSFileManager`
/// (no AppleScript, no Automation prompt).
pub struct SystemTrash;

impl Trasher for SystemTrash {
    fn trash(&self, path: &Path) -> io::Result<()> {
        trash::move_to_trash(path)
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

    /// The thread waiting for a native command sleeps between its looks: its
    /// own CPU over a 2 s command is under 1% of the wait.
    #[test]
    fn waiting_for_a_tool_does_not_spin() {
        // Where the thread clock is not ported yet; /bin/sleep is a Unix path
        // too: story "Windows scanner and rule catalog" records it.
        if crate::platform::skipped_without("waiting_for_a_tool_does_not_spin", thread_cpu_time()).is_none() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let env =
            Env::with_paths(dir.path(), &dir.path().join("data"), &dir.path().join("logs"), SystemTime::now()).unwrap();
        let (cpu, at) = (thread_cpu_time().unwrap(), Instant::now());
        let out = SystemCommands::new(&env).run(Path::new("/bin/sleep"), &["2".into()], &[], NATIVE_TIMEOUT).unwrap();
        let (cpu, wall) = (thread_cpu_time().unwrap() - cpu, at.elapsed());
        assert!(out.success);
        assert!(wall >= Duration::from_millis(1500), "{wall:?}");
        let share = cpu.as_secs_f64() / wall.as_secs_f64();
        assert!(share < 0.01, "{:.2}% CPU", share * 100.0);
    }
}
