//! Real ports: tools on disk, the macOS Trash, the process table.

use std::io::{self, Read as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{CmdOutput, CommandRunner, ProcessTable, Trasher};
use crate::env::Env;
use crate::sys::procs::{self, ProcInfo};

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

    fn run(&self, exe: &Path, args: &[String], timeout: Duration) -> io::Result<CmdOutput> {
        let mut child =
            Command::new(exe).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
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
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if started.elapsed() > timeout {
                let _ = child.kill();
                let _ = child.wait();
                return Err(io::Error::new(io::ErrorKind::TimedOut, format!("{} timed out", exe.display())));
            }
            std::thread::park_timeout(Duration::from_millis(50));
        };
        let stdout = out_t.join().unwrap_or_default();
        let stderr = err_t.join().unwrap_or_default();
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
}
