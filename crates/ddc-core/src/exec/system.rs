//! Real ports: tools on disk, the macOS Trash, the process table.

use std::io::{self, Read as _};
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{CmdOutput, CommandRunner, ProcessTable, Trasher};
use crate::env::Env;
use crate::sys::procs::{self, ProcInfo};
use crate::sys::signals;

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
        self.dirs
            .iter()
            .map(|d| d.join(program))
            .find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
    }

    /// The command gets its own process group: a timeout or a forwarded
    /// SIGINT/SIGTERM reaches everything it started, and a grandchild cannot
    /// keep the pipes open after the command is gone.
    fn run(&self, exe: &Path, args: &[String], env: &[(&str, &str)], timeout: Duration) -> io::Result<CmdOutput> {
        let mut child = Command::new(exe)
            .args(args)
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()?;
        // The group id is the child's pid; a signal that came before this is forwarded now.
        let group = i32::try_from(child.id()).unwrap_or(0);
        signals::set_child_group(group);
        // Drain both pipes on threads so a chatty command cannot block on a full pipe.
        let mut out = child.stdout.take().expect("piped");
        let mut err = child.stderr.take().expect("piped");
        let out_t = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = out.read_to_string(&mut s);
            s
        });
        let err_t = std::thread::spawn(move || {
            let mut s = String::new();
            let _ = err.read_to_string(&mut s);
            s
        });
        let started = Instant::now();
        let waited = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status),
                Ok(None) if started.elapsed() > timeout => {
                    signals::kill_group(group, libc::SIGKILL);
                    let _ = child.wait();
                    break Err(io::Error::new(io::ErrorKind::TimedOut, format!("{} timed out", exe.display())));
                }
                Ok(None) => std::thread::park_timeout(Duration::from_millis(50)),
                Err(e) => {
                    // Its pipes are drained below: a command left running would hold them open.
                    signals::kill_group(group, libc::SIGKILL);
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
}

/// The macOS Trash through `NSFileManager` (no AppleScript, no Automation prompt).
pub struct SystemTrash;

impl Trasher for SystemTrash {
    fn trash(&self, path: &Path) -> io::Result<()> {
        use trash::macos::{DeleteMethod, TrashContextExtMacos as _};
        let mut ctx = trash::TrashContext::default();
        ctx.set_delete_method(DeleteMethod::NsFileManager);
        ctx.delete(path).map_err(io::Error::other)
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
}
