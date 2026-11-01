//! Processes of the current user, for the executor's in-use and owner
//! checks, from `/proc` (safety ADR §5, amendment of 2026-10-31). What a
//! process withholds (a non-dumpable one keeps its `exe`, `cwd`, `environ`,
//! `fd` and `maps` to itself; one may exit during the read) is left empty, as
//! on macOS; its `cmdline` stays readable.

use std::ffi::{CStr, OsStr};
use std::fs;
use std::mem::MaybeUninit;
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

pub use crate::platform::types::{OpenVnode, ProcInfo, ProcStart, app_bundle};

/// Real user id of this process.
pub fn uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Home folder of the real user from the password database (and whatever
/// else NSS consults: LDAP, sssd), not `$HOME`: the load generator refuses
/// targets on its file system whatever `$HOME` says (story "Synthetic load
/// trees: large, wide, deep and hostile"). `None` for a uid with no entry,
/// such as a container's.
pub fn real_home() -> Option<PathBuf> {
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    let mut pwd = MaybeUninit::<libc::passwd>::uninit();
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    loop {
        // SAFETY: every pointer is valid for the call and `buf` outlives the
        // strings `pwd` points into, which are read before it is dropped.
        let rc = unsafe { libc::getpwuid_r(libc::getuid(), pwd.as_mut_ptr(), buf.as_mut_ptr(), buf.len(), &mut found) };
        match rc {
            // The entry does not fit: retry with more room, up to 1 MiB.
            libc::ERANGE if buf.len() < 1024 * 1024 => buf.resize(buf.len() * 4, 0),
            0 if !found.is_null() => break,
            _ => return None,
        }
    }
    // SAFETY: `found` is non-null, so `pwd` was filled and `pw_dir` is a C string in `buf`.
    let dir = unsafe { (*found).pw_dir };
    if dir.is_null() {
        return None;
    }
    // SAFETY: non-null and NUL-terminated, inside `buf`.
    let dir = unsafe { CStr::from_ptr(dir) };
    (!dir.to_bytes().is_empty()).then(|| PathBuf::from(OsStr::from_bytes(dir.to_bytes())))
}

/// Whether the process runs as root (the executor refuses to).
pub fn is_root() -> bool {
    rustix::process::geteuid().is_root()
}

/// All processes of the real user id; withheld details are left empty.
pub fn current_user_processes() -> Vec<ProcInfo> {
    user_processes(true)
}

/// [`current_user_processes`] without their open files, which it reads
/// for every process: the cost of a list that only names them.
pub fn current_user_processes_unopened() -> Vec<ProcInfo> {
    user_processes(false)
}

fn user_processes(with_files: bool) -> Vec<ProcInfo> {
    let uid = uid();
    let own = std::process::id();
    list_pids()
        .into_iter()
        .filter_map(|pid| {
            let status = Status::read(pid)?;
            (status.uid == uid).then(|| {
                let mut info = describe(pid);
                // This process and the tools it runs hold the clean's own descriptors.
                if with_files && pid as u32 != own && status.ppid != own {
                    match read_files(pid) {
                        Some(files) => info.open_files = files,
                        None => info.withheld = true,
                    }
                }
                info
            })
        })
        .collect()
}

/// Paths of the files `pid` (a process of the current user) has open, its
/// mapped files (a venv's compiled modules) included; empty when it is gone
/// or withholds them. Only files with a path count: sockets, pipes and
/// anonymous maps have none.
pub fn open_files(pid: i32) -> Vec<PathBuf> {
    match Status::read(pid) {
        Some(status) if status.uid == uid() => files(pid),
        _ => Vec::new(),
    }
}

/// The files `pid` (a process of the current user) has open, with their
/// device and inode; empty when it is gone or withholds them. Unlike the
/// snapshot, this process's children are read too.
pub fn open_vnodes(pid: i32) -> Vec<OpenVnode> {
    if !Status::read(pid).is_some_and(|s| s.uid == uid()) {
        return Vec::new();
    }
    let Ok(fds) = crate::stats::read_dir(proc(pid).join("fd")) else { return Vec::new() };
    fds.flatten()
        .filter_map(|fd| {
            // `stat` through the link reaches the open file, even an unlinked one.
            let md = fs::metadata(fd.path()).ok()?;
            (md.is_file() || md.is_dir()).then(|| OpenVnode {
                dev: md.dev(),
                ino: md.ino(),
                path: fs::read_link(fd.path()).ok().and_then(|p| file_path(p.as_os_str())),
            })
        })
        .collect()
}

/// The start of `pid`, of any user; `None` when there is no such process.
/// Linux counts it in clock ticks since boot, so it is the boot time plus
/// those ticks.
pub fn process_start(pid: i32) -> Option<ProcStart> {
    let status = Status::read(pid)?;
    let ticks = Stat::read(pid)?.start_ticks;
    let per_sec = rustix::param::clock_ticks_per_second();
    let boot = boot_time()?;
    Some(ProcStart { uid: status.uid, sec: boot + ticks / per_sec, usec: (ticks % per_sec) * 1_000_000 / per_sec })
}

/// The number of threads `pid` has now, or `None` once it is gone (or is
/// another user's).
pub fn thread_count(pid: i32) -> Option<u32> {
    Status::read(pid).filter(|s| s.uid == uid()).map(|s| s.threads)
}

fn proc(pid: i32) -> PathBuf {
    PathBuf::from(format!("/proc/{pid}"))
}

fn list_pids() -> Vec<i32> {
    let Ok(dir) = crate::stats::read_dir("/proc") else { return Vec::new() };
    dir.flatten().filter_map(|e| e.file_name().to_str()?.parse().ok()).filter(|&pid: &i32| pid > 0).collect()
}

/// What `/proc/<pid>/status` says that the checks use.
struct Status {
    uid: u32,
    ppid: u32,
    threads: u32,
}

impl Status {
    fn read(pid: i32) -> Option<Self> {
        Self::parse(&fs::read_to_string(proc(pid).join("status")).ok()?)
    }

    fn parse(text: &str) -> Option<Self> {
        let field = |key: &str| text.lines().find_map(|l| l.strip_prefix(key)).map(str::trim);
        // `Uid:` holds the real, effective, saved and file-system ids: the real one.
        let uid = field("Uid:")?.split_whitespace().next()?.parse().ok()?;
        let ppid = field("PPid:")?.parse().ok()?;
        let threads = field("Threads:").and_then(|t| t.parse().ok()).unwrap_or(1);
        Some(Self { uid, ppid, threads })
    }
}

/// What `/proc/<pid>/stat` says that the checks use.
struct Stat {
    start_ticks: u64,
}

impl Stat {
    fn read(pid: i32) -> Option<Self> {
        Self::parse(&fs::read(proc(pid).join("stat")).ok()?)
    }

    /// The command name is in parentheses and may hold spaces and
    /// parentheses itself, so the fields are counted from the last `)`:
    /// `starttime` is field 22, the 20th after it.
    fn parse(text: &[u8]) -> Option<Self> {
        let close = text.iter().rposition(|&b| b == b')')?;
        let rest = std::str::from_utf8(&text[close + 1..]).ok()?;
        Some(Self { start_ticks: rest.split_whitespace().nth(19)?.parse().ok()? })
    }
}

fn boot_time() -> Option<u64> {
    let text = fs::read_to_string("/proc/stat").ok()?;
    text.lines().find_map(|l| l.strip_prefix("btime "))?.trim().parse().ok()
}

fn describe(pid: i32) -> ProcInfo {
    let dir = proc(pid);
    // `exe` is the executable after symlinks are resolved (a venv's
    // `bin/python` reads as the base interpreter); `cmdline`'s first
    // argument keeps the path as it was run.
    let exe_link = fs::read_link(dir.join("exe"));
    let exe = exe_link.as_ref().ok().and_then(|p| file_path(p.as_os_str()));
    let mut info = ProcInfo { pid, cwd: fs::read_link(dir.join("cwd")).ok(), ..ProcInfo::default() };
    if let Ok(cmdline) = fs::read(dir.join("cmdline")) {
        info.args = split_nul(&cmdline).map(|a| OsStr::from_bytes(a).to_os_string()).collect();
    }
    let environ = fs::read(dir.join("environ"));
    if let Ok(environ) = &environ {
        info.env_prefixes = env_prefixes(environ);
    }
    // Non-dumpable, or gone during the read: what is missing could be in use.
    info.withheld = exe_link.is_err() || info.cwd.is_none() || environ.is_err();
    info.name = exe
        .as_deref()
        .or_else(|| info.args.first().map(Path::new))
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .or_else(|| fs::read_to_string(dir.join("comm")).ok().map(|c| c.trim_end().to_owned()))
        .unwrap_or_default();
    info.exe = exe;
    info
}

/// The open and the mapped files of `pid`, each path once.
fn files(pid: i32) -> Vec<PathBuf> {
    read_files(pid).unwrap_or_default()
}

/// [`files`], or `None` when the process withholds its descriptors or its
/// maps, or is gone.
fn read_files(pid: i32) -> Option<Vec<PathBuf>> {
    let dir = proc(pid);
    let fds = crate::stats::read_dir(dir.join("fd")).ok()?;
    let mut out: Vec<PathBuf> =
        fds.flatten().filter_map(|fd| fs::read_link(fd.path()).ok()).filter_map(|p| file_path(p.as_os_str())).collect();
    out.extend(mapped_files(&fs::read(dir.join("maps")).ok()?));
    out.sort();
    out.dedup();
    Some(out)
}

/// A link target that names a file: absolute, without the kernel's
/// ` (deleted)` mark. `socket:[…]`, `pipe:[…]` and `anon_inode:…` name none.
fn file_path(target: &OsStr) -> Option<PathBuf> {
    let bytes = target.as_bytes();
    let bytes = bytes.strip_suffix(b" (deleted)").unwrap_or(bytes);
    bytes.starts_with(b"/").then(|| PathBuf::from(OsStr::from_bytes(bytes)))
}

/// The paths of `maps`: the sixth field, after five that hold no spaces; a
/// path may hold spaces itself.
fn mapped_files(maps: &[u8]) -> Vec<PathBuf> {
    maps.split(|&b| b == b'\n')
        .filter_map(|line| {
            let mut rest = line;
            for _ in 0..5 {
                let at = rest.iter().position(|&b| b == b' ')?;
                rest = &rest[at + 1..];
            }
            let path = rest.iter().position(|&b| b != b' ').map(|at| &rest[at..])?;
            file_path(OsStr::from_bytes(path))
        })
        .collect()
}

fn split_nul(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.strip_suffix(b"\0").unwrap_or(bytes).split(|&b| b == 0).filter(|a| !a.is_empty())
}

fn env_prefixes(environ: &[u8]) -> Vec<PathBuf> {
    split_nul(environ)
        .filter_map(|var| {
            [&b"VIRTUAL_ENV="[..], b"CONDA_PREFIX="]
                .iter()
                .find_map(|key| var.strip_prefix(*key).filter(|v| !v.is_empty()))
                .map(|v| PathBuf::from(OsStr::from_bytes(v)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::process::{Child, Command};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::platform::testing;

    /// A spawned child, killed and reaped when the test ends.
    struct Kid(Child);

    impl Drop for Kid {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    impl Kid {
        fn pid(&self) -> i32 {
            self.0.id() as i32
        }

        /// The child once it has `exec`ed what it was asked to.
        fn waiting(self, ready: impl Fn(&ProcInfo) -> bool) -> (Self, ProcInfo) {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let seen = current_user_processes_unopened().into_iter().find(|p| p.pid == self.pid());
                if let Some(info) = seen.filter(|i| ready(i)) {
                    return (self, info);
                }
                assert!(Instant::now() < deadline, "the child never showed");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }

    #[test]
    fn status_and_stat_are_read_whatever_the_command_name_holds() {
        let s = Status::parse("Name:\tx\nPPid:\t7\nUid:\t1000\t0\t0\t0\nThreads:\t3\n").unwrap();
        assert_eq!((s.uid, s.ppid, s.threads), (1000, 7, 3));
        let stat = b"42 (a) b (c) S 1 42 42 0 -1 4194560 1 0 0 0 0 0 0 0 20 0 1 0 98765 1 2 3";
        assert_eq!(Stat::parse(stat).unwrap().start_ticks, 98765);
    }

    #[test]
    fn link_targets_maps_and_the_environment_are_read() {
        assert_eq!(file_path(OsStr::new("/a/b (deleted)")), Some(PathBuf::from("/a/b")));
        assert_eq!(file_path(OsStr::new("socket:[123]")), None);
        let maps = b"7f00-7f01 r--p 00000000 08:01 42   /p/.venv/lib/my lib.so\n7f01-7f02 rw-p 00000000 00:00 0 \n7f02-7f03 r-xp 0 00:00 0   [vdso]\n";
        assert_eq!(mapped_files(maps), vec![PathBuf::from("/p/.venv/lib/my lib.so")]);
        assert_eq!(env_prefixes(b"HOME=/h\0VIRTUAL_ENV=/p/.venv\0CONDA_PREFIX=\0"), vec![PathBuf::from("/p/.venv")]);
    }

    #[test]
    fn this_process_is_listed_with_its_executable_and_folder() {
        let me = current_user_processes().into_iter().find(|p| p.pid == std::process::id() as i32).expect("self");
        assert!(me.exe.as_deref().is_some_and(Path::is_absolute) && me.cwd.is_some(), "{me:?}");
        assert!(me.open_files.is_empty(), "its own descriptors are not read: {:?}", me.open_files);
        assert!(!me.name.is_empty());
        let start = process_start(me.pid).unwrap();
        assert_eq!(start.uid, uid());
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert!(start.sec <= now + 1 && start.sec + 86_400 > now, "{start:?}");
        assert!(thread_count(me.pid).is_some_and(|n| n >= 1));
    }

    /// A child's working folder, and a file it holds open, are seen.
    #[test]
    fn a_childs_folder_and_open_file_are_seen() {
        let d = tempfile::tempdir().unwrap();
        let held = d.path().join("held");
        fs::write(&held, b"x").unwrap();
        let child = Command::new("/bin/sh")
            .args(["-c", "exec 3<\"$0\"; exec sleep 30"])
            .arg(&held)
            .current_dir(d.path())
            .spawn()
            .unwrap();
        let (kid, info) = Kid(child).waiting(|i| i.args.first().is_some_and(|a| a == "sleep"));
        let real = crate::platform::paths::canonicalize(d.path()).unwrap();
        assert_eq!(info.cwd.as_deref(), Some(real.as_path()));
        assert!(open_files(kid.pid()).contains(&real.join("held")), "{:?}", open_files(kid.pid()));
        let vnode = fs::metadata(&held).unwrap();
        assert!(open_vnodes(kid.pid()).iter().any(|v| (v.dev, v.ino) == (vnode.dev(), vnode.ino())));
    }

    /// A venv's `bin/python` is a symlink: `exe` names the interpreter it
    /// leads to, and the first argument keeps the path inside the venv.
    #[test]
    fn a_venv_interpreter_is_found_by_its_first_argument() {
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join(".venv/bin");
        fs::create_dir_all(&bin).unwrap();
        let sleep = crate::platform::paths::canonicalize(std::path::Path::new("/bin/sleep")).unwrap();
        testing::symlink(&sleep, &bin.join("python")).unwrap();
        let child =
            Command::new(bin.join("python")).arg("30").env("VIRTUAL_ENV", d.path().join(".venv")).spawn().unwrap();
        let (_kid, info) = Kid(child).waiting(|i| i.exe.as_deref() == Some(sleep.as_path()));
        assert_eq!(info.args.first().map(PathBuf::from), Some(bin.join("python")));
        assert_eq!(info.env_prefixes, vec![d.path().join(".venv")]);
    }
}
