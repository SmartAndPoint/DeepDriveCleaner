//! How ddc starts a native command, on Windows (story "Windows scanner and
//! rule catalog", step 9c-1; safety ADR, "§5 and §6, the command runner on
//! Windows").
//!
//! Each command runs in a Job Object of its own, the counterpart of the
//! process group a command gets on Unix: [`super::signals::kill_group`]
//! ends the job, the command and everything it started.
//!
//! - The command starts suspended (`CREATE_SUSPENDED`), in a console
//!   process group of its own (`CREATE_NEW_PROCESS_GROUP`, so Ctrl-C does
//!   not reach it; Ctrl-Break and a closed console still may, which changes
//!   nothing, since ddc then ends its job), in the system folder, with
//!   `NoDefaultCurrentDirectoryInExePath=1`: Windows and `cmd.exe` look for
//!   a bare program name in the current folder first, and the folder ddc
//!   was started from is not to be trusted.
//! - It joins its job through its own handle before any of its code runs;
//!   a failed join ends it, so a command the timeout cannot reach never
//!   runs. The job is published, then every thread of the process is
//!   resumed through the process's own handle (`NtResumeProcess`; std gives
//!   no thread handle, and a thread snapshot of the whole system, the
//!   fallback, costs tens of milliseconds on every git probe). A cancel that
//!   came before the publish ends the job unresumed.
//! - The job has no `KILL_ON_JOB_CLOSE`: a command that ends normally
//!   leaves what it started, and a ddc ended from outside leaves its command
//!   running, as after `kill -9` on Unix. No breakaway is allowed, and a
//!   command that crashes ends instead of holding the item behind an error
//!   dialog (`DIE_ON_UNHANDLED_EXCEPTION`).

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::os::windows::process::CommandExt as _;
use std::path::PathBuf;
use std::process::{Child, Command};

use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;
use windows_sys::Win32::System::Threading::{
    CREATE_NEW_PROCESS_GROUP, CREATE_SUSPENDED, GetProcessIdOfThread, OpenThread, ResumeThread,
    THREAD_QUERY_LIMITED_INFORMATION, THREAD_SUSPEND_RESUME,
};

/// The extensions a program is looked for by, in this order: never
/// `PATHEXT`, never a bare name (npm puts a `sh` script beside `npm.cmd`).
pub(super) const PROGRAM_EXTENSIONS: [&str; 3] = ["exe", "cmd", "bat"];

/// The file names `program` is looked for by, in each folder of the search
/// path: `program.exe`, `program.cmd`, `program.bat`.
pub fn program_names(program: &str) -> Vec<String> {
    PROGRAM_EXTENSIONS.iter().map(|ext| format!("{program}.{ext}")).collect()
}

/// Starts `cmd` in a Job Object of its own, as the module doc says. The
/// job is published for `signals::kill_group` under the child's pid before
/// the child runs; the caller clears it once the command's pipes are
/// drained, as on Unix. The command's current folder, creation flags and
/// `NoDefaultCurrentDirectoryInExePath` are set here, over the caller's.
pub fn spawn_grouped(cmd: &mut Command) -> io::Result<Child> {
    cmd.current_dir(system_dir()?)
        .env("NoDefaultCurrentDirectoryInExePath", "1")
        .creation_flags(CREATE_SUSPENDED | CREATE_NEW_PROCESS_GROUP);
    let mut child = cmd.spawn()?;
    let job = match job_for(&child) {
        Ok(job) => job,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };
    let pid = child.id();
    if !super::signals::publish(pid, job) {
        // A cancel came first: the job is ended, the command never ran. The
        // child itself too, should ending the job have failed: a suspended
        // child no published job reaches would never end.
        let _ = child.kill();
        return Ok(child);
    }
    if let Err(e) = resume(&child) {
        super::signals::kill_group(group(pid), super::signals::SIGKILL);
        super::signals::clear_child_group(group(pid));
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }
    Ok(child)
}

fn group(pid: u32) -> i32 {
    i32::try_from(pid).unwrap_or(0)
}

/// The system folder (`GetSystemDirectoryW`), where every command starts.
pub(super) fn system_dir() -> io::Result<PathBuf> {
    let mut buf = vec![0u16; 1024];
    // SAFETY: `buf` is writable for its length in units.
    let len = unsafe { GetSystemDirectoryW(buf.as_mut_ptr(), u32::try_from(buf.len()).expect("small")) } as usize;
    if len == 0 || len >= buf.len() {
        return Err(io::Error::last_os_error());
    }
    Ok(PathBuf::from(OsString::from_wide(&buf[..len])))
}

/// A new job, its limits set, with `child` in it.
fn job_for(child: &Child) -> io::Result<OwnedHandle> {
    // SAFETY: no security attributes, no name: an unnamed job nobody inherits.
    let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a handle CreateJobObjectW just returned, owned by nobody else.
    let job = unsafe { OwnedHandle::from_raw_handle(raw) };
    // SAFETY: plain data; all zeroes is a valid value.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION;
    let size = u32::try_from(size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>()).expect("a small struct");
    // SAFETY: the job is open; `limits` is the class's struct, `size` bytes.
    let set = unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            size,
        )
    };
    if set == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both handles are open: the job, and the child's own process handle.
    if unsafe { AssignProcessToJobObject(job.as_raw_handle(), child.as_raw_handle()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(job)
}

/// `NtResumeProcess`: resumes every thread of a process through its handle.
/// Not in the SDK, exported by ntdll since Windows XP (Process Explorer and
/// PsSuspend use it); looked up once.
type NtResumeProcess = unsafe extern "system" fn(windows_sys::Win32::Foundation::HANDLE) -> i32;

fn nt_resume_process() -> Option<NtResumeProcess> {
    static RESUME: std::sync::OnceLock<Option<NtResumeProcess>> = std::sync::OnceLock::new();
    *RESUME.get_or_init(|| {
        let ntdll: Vec<u16> = "ntdll.dll".encode_utf16().chain([0]).collect();
        // SAFETY: a NUL-terminated wide name; ntdll is mapped in every process
        // and never unloaded, so the module handle needs no release.
        let module = unsafe { GetModuleHandleW(ntdll.as_ptr()) };
        if module.is_null() {
            return None;
        }
        // SAFETY: a NUL-terminated ASCII name and a module handle that stays
        // valid for the process's life.
        let found = unsafe { GetProcAddress(module, c"NtResumeProcess".as_ptr().cast()) }?;
        // SAFETY: the export takes a process handle and returns an NTSTATUS;
        // both are `extern "system"` function pointers of the same size.
        Some(unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, NtResumeProcess>(found) })
    })
}

/// Resumes every thread of the suspended `child`: through its own handle
/// (`NtResumeProcess`), which needs no thread snapshot of the whole system
/// (tens of milliseconds, on every git probe); through the snapshot where
/// ntdll does not export it or the call fails. A failed call resumed no
/// thread; a thread resumed twice is resumed once (its count stops at 0).
fn resume(child: &Child) -> io::Result<()> {
    let mut failed = None;
    if let Some(resume_process) = nt_resume_process() {
        // SAFETY: the child's own handle, open with every right, for the call.
        let status = unsafe { resume_process(child.as_raw_handle()) };
        if status >= 0 {
            return Ok(());
        }
        failed = Some(status);
    }
    resume_threads(child.id()).map_err(|e| match failed {
        Some(status) => io::Error::new(e.kind(), format!("NtResumeProcess failed ({status:#x}), then {e}")),
        None => e,
    })
}

/// Resumes every thread of the suspended process `pid`, found through a
/// thread snapshot.
fn resume_threads(pid: u32) -> io::Result<()> {
    // SAFETY: plain arguments; the handle is checked below.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a handle CreateToolhelp32Snapshot just returned, owned by nobody else.
    let snap = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry =
        THREADENTRY32 { dwSize: u32::try_from(size_of::<THREADENTRY32>()).expect("small"), ..Default::default() };
    let mut resumed = 0;
    // SAFETY: the snapshot is open; `entry` is a local with its size set.
    let mut more = unsafe { Thread32First(snap.as_raw_handle(), &mut entry) } != 0;
    while more {
        if entry.th32OwnerProcessID == pid {
            resume_thread(pid, entry.th32ThreadID)?;
            resumed += 1;
        }
        // SAFETY: as for Thread32First.
        more = unsafe { Thread32Next(snap.as_raw_handle(), &mut entry) } != 0;
    }
    if resumed == 0 {
        return Err(io::Error::other(format!("no thread of process {pid} to resume")));
    }
    Ok(())
}

/// Resumes the thread `tid`, once it is checked to be `pid`'s (a thread id
/// can be reused).
fn resume_thread(pid: u32, tid: u32) -> io::Result<()> {
    // SAFETY: plain arguments; the handle is checked below.
    let raw = unsafe { OpenThread(THREAD_SUSPEND_RESUME | THREAD_QUERY_LIMITED_INFORMATION, 0, tid) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a handle OpenThread just returned, owned by nobody else.
    let thread = unsafe { OwnedHandle::from_raw_handle(raw) };
    // SAFETY: the thread handle is open for the call.
    if unsafe { GetProcessIdOfThread(thread.as_raw_handle()) } != pid {
        return Err(io::Error::other(format!("thread {tid} is not process {pid}'s")));
    }
    // SAFETY: as above.
    if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    use super::*;

    fn cmd(args: &[&str]) -> Command {
        let mut cmd = Command::new(system_dir().unwrap().join("cmd.exe"));
        cmd.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        cmd
    }

    fn output(child: &mut Child) -> String {
        let mut text = String::new();
        child.stdout.take().unwrap().read_to_string(&mut text).unwrap();
        text
    }

    #[test]
    fn a_program_is_looked_for_as_exe_then_cmd_then_bat() {
        assert_eq!(program_names("uv"), ["uv.exe", "uv.cmd", "uv.bat"]);
    }

    /// Only a file named `.exe`, `.cmd` or `.bat` runs: a script of another
    /// kind, a bare name and a folder named like a program do not.
    #[test]
    fn only_exe_cmd_and_bat_files_run() {
        let d = tempfile::tempdir().unwrap();
        for name in ["a.exe", "b.CMD", "c.bat", "d.ps1", "e.com", "npm"] {
            std::fs::write(d.path().join(name), b"").unwrap();
        }
        std::fs::create_dir(d.path().join("f.cmd")).unwrap();
        let runs = |name: &str| {
            let path = d.path().join(name);
            super::super::meta::is_executable(&path, &std::fs::metadata(&path).unwrap())
        };
        assert!(runs("a.exe") && runs("b.CMD") && runs("c.bat"));
        assert!(!runs("d.ps1") && !runs("e.com") && !runs("npm") && !runs("f.cmd"));
    }

    /// A batch file gets an argument with `&` as one argument, quoted, never
    /// as a second command on the line `cmd.exe` runs (std's quoting): the
    /// script echoes it as it came (`%1`, quotes kept), on one line.
    #[test]
    fn a_batch_file_takes_an_ampersand_as_one_argument() {
        let d = tempfile::tempdir().unwrap();
        let script = d.path().join("echo-arg.cmd");
        std::fs::write(&script, "@echo off\r\necho [%1]\r\n").unwrap();
        let mut command = Command::new(&script);
        command.arg("x&echo hit").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = spawn_grouped(&mut command).unwrap();
        let text = output(&mut child);
        assert!(child.wait().unwrap().success());
        super::super::signals::clear_child_group(group(child.id()));
        assert_eq!(text.trim(), r#"["x&echo hit"]"#, "one argument, no second command");
    }

    /// A line break in a batch file's argument is refused before anything
    /// starts.
    #[test]
    fn a_line_break_for_a_batch_file_is_refused_before_it_starts() {
        let d = tempfile::tempdir().unwrap();
        let script = d.path().join("x.cmd");
        std::fs::write(&script, "@echo off\r\n").unwrap();
        let mut command = Command::new(&script);
        command.arg("a\nb").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        assert_eq!(spawn_grouped(&mut command).unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert!(super::super::signals::child_groups().is_empty(), "nothing published");
    }

    /// The command runs once resumed, in its job, from the system folder,
    /// with no program found in the current folder.
    #[test]
    fn a_command_runs_in_its_job_from_the_system_folder() {
        let mut child = spawn_grouped(&mut cmd(&["/C", "cd& echo %NoDefaultCurrentDirectoryInExePath%"])).unwrap();
        let pid = group(child.id());
        assert!(super::super::signals::child_groups().contains(&pid), "published before it runs");
        let text = output(&mut child);
        assert!(child.wait().unwrap().success());
        super::super::signals::clear_child_group(pid);
        let mut lines = text.lines();
        let cwd = PathBuf::from(lines.next().unwrap().trim());
        assert_eq!(super::super::paths::key(&cwd), super::super::paths::key(&system_dir().unwrap()));
        assert_eq!(lines.next().map(str::trim), Some("1"));
        assert!(!super::super::signals::child_groups().contains(&pid), "cleared");
    }

    /// The resume through the process's own handle is there: without it
    /// every command pays a thread snapshot of the whole system.
    #[test]
    fn ntdll_resumes_a_process_through_its_handle() {
        assert!(nt_resume_process().is_some());
    }

    /// The job holds exactly the limits the ADR names: no kill on close, no
    /// breakaway, a crash ends the command.
    #[test]
    fn the_job_has_only_the_crash_limit() {
        let mut child = spawn_grouped(&mut cmd(&["/C", "echo x"])).unwrap();
        let pid = group(child.id());
        let flags = super::super::signals::limit_flags(pid);
        let _ = output(&mut child);
        let _ = child.wait();
        super::super::signals::clear_child_group(pid);
        assert_eq!(flags, Some(JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION));
    }

    /// The fallback through a thread snapshot resumes a suspended process too.
    #[test]
    fn the_thread_snapshot_resumes_a_process_too() {
        let mut command = cmd(&["/C", "echo resumed"]);
        command.creation_flags(CREATE_SUSPENDED);
        let mut child = command.spawn().unwrap();
        if let Err(e) = resume_threads(child.id()) {
            let _ = child.kill();
            panic!("{e}");
        }
        assert_eq!(output(&mut child).trim(), "resumed");
        assert!(child.wait().unwrap().success());
    }

    /// Ending the job ends the command and what it started.
    #[test]
    fn ending_the_job_ends_the_command_and_its_children() {
        let mut child = spawn_grouped(&mut cmd(&["/C", "ping -n 60 127.0.0.1 >NUL"])).unwrap();
        let pid = group(child.id());
        let started = Instant::now();
        while super::super::signals::active_processes(pid) < 2 && started.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(super::super::signals::active_processes(pid), 2, "cmd and its ping");
        super::super::signals::kill_group(pid, super::super::signals::SIGKILL);
        let waited = Instant::now();
        let status = child.wait().unwrap();
        assert!(!status.success() && waited.elapsed() < Duration::from_secs(5));
        let ended = Instant::now();
        while super::super::signals::active_processes(pid) > 0 && ended.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(super::super::signals::active_processes(pid), 0, "the ping ended too");
        super::super::signals::clear_child_group(pid);
    }

    /// Ctrl-C ends a running command's job at once.
    #[test]
    fn ctrl_c_ends_a_running_command() {
        let mut child = spawn_grouped(&mut cmd(&["/C", "ping -n 60 127.0.0.1 >NUL"])).unwrap();
        let started = Instant::now();
        super::super::signals::cancel(super::super::signals::signal_of(0).unwrap());
        let status = child.wait().unwrap();
        assert!(!status.success() && started.elapsed() < Duration::from_secs(5));
        super::super::signals::clear_child_group(group(child.id()));
    }

    /// The search finds `.exe` before `.cmd` and never a bare name; a
    /// folder that is not absolute is never searched.
    #[test]
    fn the_search_takes_exe_then_cmd_and_no_relative_folder() {
        use crate::exec::{CommandRunner as _, SystemCommands};
        let d = tempfile::tempdir().unwrap();
        let found = |names: &[&str]| {
            let bin = d.path().join(names.join("+"));
            std::fs::create_dir(&bin).unwrap();
            for name in names {
                std::fs::write(bin.join(name), b"").unwrap();
            }
            let env = crate::env::Env::with_paths(
                d.path(),
                &d.path().join("data"),
                &d.path().join("logs"),
                std::time::SystemTime::now(),
            )
            .unwrap()
            .with_search_path(vec![PathBuf::from("rel"), bin.clone()]);
            assert!(env.tool_dirs().iter().all(|p| p.is_absolute()), "{:?}", env.tool_dirs());
            SystemCommands::new(&env).find("tool").map(|p| p.strip_prefix(&bin).unwrap().to_path_buf())
        };
        assert_eq!(found(&["tool", "tool.cmd", "tool.exe"]), Some(PathBuf::from("tool.exe")));
        assert_eq!(found(&["tool", "tool.cmd"]), Some(PathBuf::from("tool.cmd")));
        assert_eq!(found(&["tool"]), None, "never a bare name");
    }

    /// A cancel that came before the command was published ends it
    /// unresumed: it never runs.
    #[test]
    fn a_cancel_before_the_start_means_the_command_never_runs() {
        let d = tempfile::tempdir().unwrap();
        let marker = d.path().join("ran");
        super::super::signals::cancel(super::super::signals::SIGINT);
        let line = format!("echo x> \"{}\"", marker.display());
        let mut child = spawn_grouped(&mut cmd(&["/C", &line])).unwrap();
        let status = child.wait().unwrap();
        assert!(!status.success());
        assert!(!marker.exists(), "the command never ran");
        assert!(!super::super::signals::child_groups().contains(&group(child.id())), "never published");
    }
}
