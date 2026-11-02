//! Processes of the current user, for the executor's in-use and owner checks.

use std::ffi::{CStr, OsStr};
use std::io;
use std::mem::{MaybeUninit, size_of};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use libc::{c_int, c_void};

pub use crate::platform::types::{OpenVnode, ProcInfo, ProcStart, RootOwner, app_bundle};

/// Real user id of this process.
pub fn uid() -> u32 {
    // SAFETY: no arguments, cannot fail.
    unsafe { libc::getuid() }
}

/// Home folder of the real user from the password database, not `$HOME`:
/// the load generator refuses targets on its file system whatever `$HOME`
/// says (story "Synthetic load trees: large, wide, deep and hostile").
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
    // SAFETY: no arguments, cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// Every root is the user's to scan: root itself is refused by [`is_root`]
/// where it matters, and the owner check of an elevated session is
/// Windows'. Reads nothing.
pub fn root_owner(_root: &Path) -> io::Result<RootOwner> {
    Ok(RootOwner::Own)
}

/// Windows' privileges that pass every ACL have no counterpart here: root
/// is refused instead. Does nothing.
pub fn drop_acl_bypass_privileges() -> io::Result<()> {
    Ok(())
}

/// All processes of the real user id; unreadable details are left empty.
pub fn current_user_processes() -> Vec<ProcInfo> {
    user_processes(true)
}

/// [`current_user_processes`] without their open files, which it reads
/// for every process: the cost of a list that only names them.
pub fn current_user_processes_unopened() -> Vec<ProcInfo> {
    user_processes(false)
}

fn user_processes(with_files: bool) -> Vec<ProcInfo> {
    // SAFETY: no arguments, cannot fail.
    let uid = unsafe { libc::getuid() };
    let argmax = argmax().unwrap_or(1 << 20);
    let own = std::process::id();
    list_pids()
        .into_iter()
        .filter(|&pid| pid > 0)
        .filter_map(|pid| {
            let bsd = bsd_info(pid)?;
            (bsd.pbi_uid == uid).then(|| {
                let mut info = describe(pid, &bsd, argmax);
                // This process and the tools it runs hold the clean's own descriptors.
                if with_files && pid as u32 != own && bsd.pbi_ppid != own {
                    info.open_files = vnode_files(pid, bsd.pbi_nfiles);
                }
                info
            })
        })
        .collect()
}

/// Paths of the files `pid` (a process of the current user) has open; empty
/// when it is gone or unreadable. Only vnodes count: sockets and pipes have no
/// path. The paths are the kernel's (`/private/var/…`, never `/var/…`) and are
/// never `stat`ed here: a file in a protected folder could make macOS ask.
pub fn open_files(pid: i32) -> Vec<PathBuf> {
    match bsd_info(pid) {
        Some(bsd) if bsd.pbi_uid == uid() => vnode_files(pid, bsd.pbi_nfiles),
        _ => Vec::new(),
    }
}

/// The files `pid` (a process of the current user) has open, with their
/// device and inode; empty when it is gone or unreadable. Unlike the
/// snapshot, this process's children are read too.
pub fn open_vnodes(pid: i32) -> Vec<OpenVnode> {
    match bsd_info(pid) {
        Some(bsd) if bsd.pbi_uid == uid() => list_fds(pid, bsd.pbi_nfiles)
            .into_iter()
            .filter(|fd| fd.proc_fdtype == libc::PROX_FDTYPE_VNODE as u32)
            .filter_map(|fd| fd_vnode(pid, fd.proc_fd))
            .collect(),
        _ => Vec::new(),
    }
}

/// The start of `pid`, of any user; `None` when there is no such process.
/// A process of the user is read through `PROC_PIDTBSDINFO`; another user's
/// refuses that flavour, so its owner comes from the short BSD info and its
/// start from `sysctl(KERN_PROC_PID)`, which every user may read.
pub fn process_start(pid: i32) -> Option<ProcStart> {
    if let Some(bsd) = bsd_info(pid) {
        return Some(ProcStart { uid: bsd.pbi_uid, sec: bsd.pbi_start_tvsec, usec: bsd.pbi_start_tvusec });
    }
    let short = short_info(pid)?;
    let (sec, usec) = start_time(pid)?;
    Some(ProcStart { uid: short.pbsi_uid, sec, usec })
}

/// `PROC_PIDT_SHORTBSDINFO`: the owner and the name, for any user's process.
fn short_info(pid: i32) -> Option<libc::proc_bsdshortinfo> {
    let mut info = MaybeUninit::<libc::proc_bsdshortinfo>::zeroed();
    let size = size_of::<libc::proc_bsdshortinfo>() as c_int;
    // SAFETY: `info` is writable for `size` bytes.
    let r = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDT_SHORTBSDINFO, 0, info.as_mut_ptr().cast(), size) };
    // SAFETY: proc_pidinfo filled the struct when it returned its size.
    (r == size).then(|| unsafe { info.assume_init() })
}

/// The start of `pid` from `sysctl(KERN_PROC_PID)`: `kinfo_proc` begins with
/// `extern_proc`'s `p_un`, a union whose `p_starttime` is a `timeval` at
/// offset 0 (`tv_sec` 8 bytes, then `tv_usec` 4). `libc` does not declare
/// `kinfo_proc`, so the buffer is sized by the kernel and read there.
fn start_time(pid: i32) -> Option<(u64, u64)> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_PID, pid];
    let mut size = 0usize;
    // SAFETY: a null buffer asks for the size.
    let r = unsafe { libc::sysctl(mib.as_mut_ptr(), 4, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) };
    if r != 0 || size < 16 {
        return None;
    }
    let mut buf = vec![0u8; size];
    // SAFETY: valid mib; `buf` is writable for `size` bytes.
    let r = unsafe { libc::sysctl(mib.as_mut_ptr(), 4, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) };
    // No such process: the call succeeds with nothing written.
    if r != 0 || size < 16 {
        return None;
    }
    let sec = i64::from_ne_bytes(buf[0..8].try_into().ok()?);
    let usec = i32::from_ne_bytes(buf[8..12].try_into().ok()?);
    Some((u64::try_from(sec).ok()?, u64::try_from(usec).ok()?))
}

fn vnode_files(pid: i32, nfiles: u32) -> Vec<PathBuf> {
    list_fds(pid, nfiles)
        .into_iter()
        .filter(|fd| fd.proc_fdtype == libc::PROX_FDTYPE_VNODE as u32)
        .filter_map(|fd| fd_vnode(pid, fd.proc_fd)?.path)
        .collect()
}

/// The descriptor table, sized from `pbi_nfiles` with slack; a full buffer
/// means it grew meanwhile, so the read is repeated once at double the size,
/// and what that read returns is kept.
fn list_fds(pid: i32, nfiles: u32) -> Vec<libc::proc_fdinfo> {
    let mut capacity = nfiles as usize + 32;
    for attempt in 0..2 {
        let mut fds = vec![libc::proc_fdinfo { proc_fd: 0, proc_fdtype: 0 }; capacity];
        let bytes = (capacity * size_of::<libc::proc_fdinfo>()) as c_int;
        // SAFETY: `fds` is writable for `bytes` bytes.
        let r = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, fds.as_mut_ptr().cast(), bytes) };
        if r <= 0 {
            return Vec::new();
        }
        fds.truncate(r as usize / size_of::<libc::proc_fdinfo>());
        if fds.len() < capacity || attempt == 1 {
            return fds;
        }
        capacity *= 2;
    }
    Vec::new()
}

// <sys/proc_info.h>, not in libc 0.2.191.
const PROC_PIDFDVNODEPATHINFO: c_int = 2;

#[repr(C)]
struct ProcFileInfo {
    fi_openflags: u32,
    fi_status: u32,
    fi_offset: libc::off_t,
    fi_type: i32,
    fi_guardflags: u32,
}

#[repr(C)]
struct VnodeFdInfoWithPath {
    pfi: ProcFileInfo,
    pvip: libc::vnode_info_path,
}

const _: () = assert!(size_of::<VnodeFdInfoWithPath>() == 1200);

/// Descriptor `fd`'s vnode; `None` when it closed since the table was read.
/// Its path is `None` when the file has no path any more (unlinked).
fn fd_vnode(pid: i32, fd: i32) -> Option<OpenVnode> {
    let mut info = MaybeUninit::<VnodeFdInfoWithPath>::zeroed();
    let size = size_of::<VnodeFdInfoWithPath>() as c_int;
    // SAFETY: `info` is writable for `size` bytes.
    let r = unsafe { libc::proc_pidfdinfo(pid, fd, PROC_PIDFDVNODEPATHINFO, info.as_mut_ptr().cast(), size) };
    if r != size {
        return None;
    }
    // SAFETY: filled by the kernel; `vip_path` is NUL-terminated.
    let info = unsafe { info.assume_init() };
    // SAFETY: as above.
    let path = unsafe { CStr::from_ptr(info.pvip.vip_path.as_ptr().cast()) };
    let stat = &info.pvip.vip_vi.vi_stat;
    Some(OpenVnode {
        // `st_dev` is an `i32`; std widens it with its sign.
        dev: stat.vst_dev as i32 as i64 as u64,
        ino: stat.vst_ino,
        path: (!path.to_bytes().is_empty()).then(|| PathBuf::from(OsStr::from_bytes(path.to_bytes()))),
    })
}

fn list_pids() -> Vec<i32> {
    // SAFETY: a null buffer asks for the number of pids.
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if n <= 0 {
        return Vec::new();
    }
    let mut pids = vec![0i32; n as usize + 256];
    let bytes = (pids.len() * size_of::<i32>()) as c_int;
    // SAFETY: `pids` is writable for `bytes` bytes.
    let n = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast::<c_void>(), bytes) };
    pids.truncate(n.max(0) as usize);
    pids
}

/// The number of threads `pid` has now, or `None` once it is gone (or is
/// another user's).
pub fn thread_count(pid: i32) -> Option<u32> {
    let mut info = MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let size = size_of::<libc::proc_taskinfo>() as c_int;
    // SAFETY: `info` is writable for `size` bytes.
    let r = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTASKINFO, 0, info.as_mut_ptr().cast(), size) };
    // SAFETY: proc_pidinfo filled the struct when it returned its size.
    (r == size).then(|| u32::try_from(unsafe { info.assume_init() }.pti_threadnum).unwrap_or(0))
}

fn bsd_info(pid: i32) -> Option<libc::proc_bsdinfo> {
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = size_of::<libc::proc_bsdinfo>() as c_int;
    // SAFETY: `info` is writable for `size` bytes.
    let r = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast(), size) };
    // SAFETY: proc_pidinfo filled the struct when it returned its size.
    (r == size).then(|| unsafe { info.assume_init() })
}

fn cwd(pid: i32) -> Option<PathBuf> {
    let mut info = MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let size = size_of::<libc::proc_vnodepathinfo>() as c_int;
    // SAFETY: `info` is writable for `size` bytes.
    let r = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, info.as_mut_ptr().cast(), size) };
    if r != size {
        return None;
    }
    // SAFETY: filled by the kernel; `vip_path` is NUL-terminated.
    let info = unsafe { info.assume_init() };
    // SAFETY: as above.
    let path = unsafe { CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast()) };
    (!path.to_bytes().is_empty()).then(|| PathBuf::from(OsStr::from_bytes(path.to_bytes())))
}

fn argmax() -> Option<usize> {
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    let mut value: c_int = 0;
    let mut size = size_of::<c_int>();
    // SAFETY: valid mib, out-pointer and size.
    let r = unsafe { libc::sysctl(mib.as_mut_ptr(), 2, (&raw mut value).cast(), &mut size, std::ptr::null_mut(), 0) };
    (r == 0 && value > 0).then_some(value as usize)
}

/// `KERN_PROCARGS2`: `int argc`, exec path, NUL padding, argv, environment.
fn procargs(pid: i32, argmax: usize) -> Option<Vec<u8>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut buf = vec![0u8; argmax];
    let mut size = buf.len();
    // SAFETY: valid mib; `buf` is writable for `size` bytes.
    let r = unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) };
    if r != 0 {
        return None;
    }
    buf.truncate(size);
    Some(buf)
}

/// The kernel's name of a process: `pbi_name`, else `pbi_comm`.
fn kernel_name(bsd: &libc::proc_bsdinfo) -> String {
    // SAFETY: both name arrays are NUL-terminated by the kernel.
    let name = unsafe { CStr::from_ptr(bsd.pbi_name.as_ptr()) };
    // SAFETY: as above.
    let name = if name.to_bytes().is_empty() { unsafe { CStr::from_ptr(bsd.pbi_comm.as_ptr()) } } else { name };
    name.to_string_lossy().into_owned()
}

fn describe(pid: i32, bsd: &libc::proc_bsdinfo, argmax: usize) -> ProcInfo {
    let mut info =
        ProcInfo { pid, name: kernel_name(bsd), cwd: cwd(pid), resolved_exe: pidpath(pid), ..ProcInfo::default() };
    if let Some(buf) = procargs(pid, argmax) {
        parse_procargs(&buf, &mut info);
    }
    info
}

/// The executable `pid` runs, every symlink resolved (`proc_pidpath`):
/// `KERN_PROCARGS2` keeps the path passed to `execve`, which for Claude
/// Code is its launcher link, not the build it runs.
fn pidpath(pid: i32) -> Option<PathBuf> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `buf` is writable for its length, which is passed.
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast::<c_void>(), buf.len() as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    (buf.first() == Some(&b'/')).then(|| PathBuf::from(OsStr::from_bytes(&buf)))
}

/// The names of the processes of other users, as the kernel keeps them
/// (`pbsi_comm`, from the short BSD info, the one flavour another user's
/// process gives without root): its arguments and executable cannot be
/// read (safety ADR §5, amendment of 2026-11-02).
pub fn other_user_names() -> Vec<String> {
    let me = uid();
    let mut names: Vec<String> = list_pids()
        .into_iter()
        .filter(|&pid| pid > 0)
        .filter_map(|pid| short_info(pid).filter(|s| s.pbsi_uid != me))
        .map(|s| {
            // SAFETY: the kernel NUL-terminates `pbsi_comm`.
            let name = unsafe { CStr::from_ptr(s.pbsi_comm.as_ptr()) };
            name.to_string_lossy().into_owned()
        })
        .filter(|n| !n.is_empty())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// macOS has no `hidepid`: every user may list every process.
pub fn process_table_hidden() -> bool {
    false
}

pub(crate) fn parse_procargs(buf: &[u8], info: &mut ProcInfo) {
    let Some(argc) = buf.get(..4).map(|b| i32::from_ne_bytes(b.try_into().expect("4 bytes"))) else { return };
    let mut rest = &buf[4..];
    let next = |rest: &mut &[u8]| -> Option<Vec<u8>> {
        let end = rest.iter().position(|&b| b == 0)?;
        let s = rest[..end].to_vec();
        *rest = &rest[end + 1..];
        Some(s)
    };
    let Some(exe) = next(&mut rest) else { return };
    if !exe.is_empty() {
        let exe = PathBuf::from(OsStr::from_bytes(&exe));
        if let Some(base) = exe.file_name() {
            info.name = base.to_string_lossy().into_owned();
        }
        info.exe = Some(exe);
    }
    while rest.first() == Some(&0) {
        rest = &rest[1..];
    }
    for _ in 0..argc.max(0) {
        match next(&mut rest) {
            Some(arg) => info.args.push(OsStr::from_bytes(&arg).to_os_string()),
            None => return,
        }
    }
    while let Some(var) = next(&mut rest) {
        if var.is_empty() {
            break;
        }
        for key in [&b"VIRTUAL_ENV="[..], b"CONDA_PREFIX="] {
            if let Some(value) = var.strip_prefix(key).filter(|v| !v.is_empty()) {
                info.env_prefixes.push(PathBuf::from(OsStr::from_bytes(value)));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::Canonical as _;

    /// `proc_pidpath` gives the executable this process runs, resolved.
    #[test]
    fn this_process_runs_its_resolved_executable() {
        let bsd = bsd_info(std::process::id() as i32).expect("this process");
        let me = describe(std::process::id() as i32, &bsd, argmax().unwrap_or(1 << 20));
        let exe = std::env::current_exe().unwrap().canonical().unwrap();
        assert_eq!(me.resolved_exe.as_deref(), Some(exe.as_path()));
    }

    /// A process started through a symlink: its `execve` path is the link,
    /// and `proc_pidpath` names the file it runs, as Claude Code's launcher
    /// runs a build (safety ADR §5, amendment of 2026-11-02).
    #[test]
    fn a_process_run_through_a_link_names_the_file_it_runs() {
        let d = tempfile::tempdir().unwrap();
        let link = d.path().canonical().unwrap().join("claude");
        std::os::unix::fs::symlink("/bin/sleep", &link).unwrap();
        let mut child = std::process::Command::new(&link).arg("30").spawn().unwrap();
        let pid = child.id() as i32;
        // Until the child has exec'ed: before, it is a copy of this process.
        let seen = (0..100).find_map(|_| {
            let found = current_user_processes_unopened()
                .into_iter()
                .find(|p| p.pid == pid && p.exe.as_deref() == Some(link.as_path()));
            if found.is_none() {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            found
        });
        child.kill().unwrap();
        child.wait().unwrap();
        let seen = seen.expect("the child is listed");
        assert_eq!(seen.exe.as_deref(), Some(link.as_path()), "the path passed to execve");
        assert_eq!(seen.resolved_exe.as_deref(), Some(std::path::Path::new("/bin/sleep")), "the file it runs");
    }

    /// launchd is root's: every user may read its name and its start, and
    /// this process's start reads as it did through the full BSD info.
    #[test]
    fn other_users_processes_are_named_and_started() {
        assert!(other_user_names().iter().any(|n| n == "launchd"), "{:?}", other_user_names());
        assert!(!process_table_hidden());
        let launchd = process_start(1).expect("launchd's start");
        assert_eq!(launchd.uid, 0);
        assert!(launchd.sec > 0);
        let me = std::process::id() as i32;
        let bsd = bsd_info(me).unwrap();
        let (sec, usec) = start_time(me).expect("this process's start by sysctl");
        assert_eq!((sec, usec), (bsd.pbi_start_tvsec, bsd.pbi_start_tvusec), "both read p_start");
    }

    #[test]
    fn network_file_systems_and_fuse_count_as_network() {
        for t in ["nfs", "smbfs", "afpfs", "webdav", "acfs", "macfuse", "osxfuse", "fuse-t"] {
            assert!(crate::platform::statfs::is_network_fs(t), "{t}");
        }
        for t in ["apfs", "hfs", "msdos", "exfat", "devfs"] {
            assert!(!crate::platform::statfs::is_network_fs(t), "{t}");
        }
    }

    #[test]
    fn real_home_is_an_absolute_folder() {
        let home = real_home().expect("the password database names a home");
        assert!(home.is_absolute(), "{home:?}");
    }

    #[test]
    fn procargs_layout_is_parsed() {
        let mut buf = 2i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"./.venv/bin/python\0\0\0\0python\0app.py\0HOME=/h\0VIRTUAL_ENV=/p/.venv\0\0");
        let mut info = ProcInfo::default();
        parse_procargs(&buf, &mut info);
        assert_eq!(info.exe, Some(PathBuf::from("./.venv/bin/python")));
        assert_eq!(info.name, "python");
        assert_eq!(info.args, vec![std::ffi::OsString::from("python"), std::ffi::OsString::from("app.py")]);
        assert_eq!(info.env_prefixes, vec![PathBuf::from("/p/.venv")]);
    }

    #[test]
    fn this_process_is_listed_with_its_executable() {
        let me = current_user_processes().into_iter().find(|p| p.pid == std::process::id() as i32).expect("self");
        assert!(me.exe.is_some() && me.cwd.is_some(), "{me:?}");
        assert!(me.open_files.is_empty(), "its own descriptors are not read: {:?}", me.open_files);
    }

    /// A child holding a file open: `open_files` sees it by pid; the snapshot
    /// leaves it out, since the clean's own tools are its children.
    #[test]
    fn open_files_of_a_process_are_read_by_pid_and_not_for_children() {
        let dir = tempfile::tempdir().unwrap();
        let held = dir.path().canonical().unwrap().join("held.bin");
        std::fs::write(&held, b"x").unwrap();
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .stdin(std::fs::File::open(&held).unwrap())
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let seen = open_files(pid);
        let listed = current_user_processes().into_iter().find(|p| p.pid == pid);
        let _ = child.kill();
        let _ = child.wait();
        assert!(seen.contains(&held), "{held:?} not in {seen:?}");
        let listed = listed.expect("the child is listed");
        assert!(listed.exe.is_some() && listed.open_files.is_empty(), "{listed:?}");
    }

    /// The kernel's `(dev, ino)` of an open file equals the one `stat` gives.
    #[test]
    fn open_vnodes_of_a_child_carry_the_stat_identity() {
        use std::os::unix::fs::MetadataExt as _;
        let dir = tempfile::tempdir().unwrap();
        let held = dir.path().join("held.lock");
        std::fs::write(&held, b"").unwrap();
        let meta = std::fs::metadata(&held).unwrap();
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .stdin(std::fs::File::open(&held).unwrap())
            .spawn()
            .unwrap();
        let seen = open_vnodes(child.id() as i32);
        let _ = child.kill();
        let _ = child.wait();
        assert!(seen.iter().any(|v| v.dev == meta.dev() && v.ino == meta.ino()), "{meta:?} not in {seen:?}");
    }

    #[test]
    fn the_thread_count_includes_threads_just_started() {
        let n = 8;
        let ready = std::sync::Barrier::new(n + 1);
        let done = std::sync::Barrier::new(n + 1);
        std::thread::scope(|s| {
            for _ in 0..n {
                s.spawn(|| {
                    ready.wait();
                    done.wait();
                });
            }
            ready.wait();
            // Other tests' threads come and go; these eight and this one are certain.
            let count = thread_count(std::process::id() as i32).unwrap();
            done.wait();
            assert!(count as usize > n, "{count}");
        });
        assert_eq!(thread_count(-1), None);
    }

    #[test]
    fn this_process_started_before_now_as_this_user() {
        let start = process_start(std::process::id() as i32).unwrap();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(start.uid, uid());
        assert!(start.sec > 0 && start.sec <= now && start.usec < 1_000_000);
    }
}
