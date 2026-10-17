//! Processes of the current user, for the executor's in-use and owner checks.

use std::ffi::{CStr, OsStr, OsString};
use std::mem::{MaybeUninit, size_of};
use std::os::unix::ffi::OsStrExt as _;
use std::path::PathBuf;

use libc::{c_int, c_void};

/// What a process could be using.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcInfo {
    pub pid: i32,
    /// Executable name (basename of the exec path, or the kernel's name).
    pub name: String,
    /// The path passed to `execve`, before symlinks are resolved; may be relative.
    pub exe: Option<PathBuf>,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// `VIRTUAL_ENV` / `CONDA_PREFIX` from the environment, when readable.
    pub env_prefixes: Vec<PathBuf>,
}

/// Real user id of this process.
pub fn uid() -> u32 {
    // SAFETY: no arguments, cannot fail.
    unsafe { libc::getuid() }
}

/// Whether the process runs as root (the executor refuses to).
pub fn is_root() -> bool {
    // SAFETY: no arguments, cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// All processes of the real user id; unreadable details are left empty.
pub fn current_user_processes() -> Vec<ProcInfo> {
    // SAFETY: no arguments, cannot fail.
    let uid = unsafe { libc::getuid() };
    let argmax = argmax().unwrap_or(1 << 20);
    list_pids()
        .into_iter()
        .filter(|&pid| pid > 0)
        .filter_map(|pid| {
            let bsd = bsd_info(pid)?;
            (bsd.pbi_uid == uid).then(|| describe(pid, &bsd, argmax))
        })
        .collect()
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

fn describe(pid: i32, bsd: &libc::proc_bsdinfo, argmax: usize) -> ProcInfo {
    let kernel_name = {
        // SAFETY: both name arrays are NUL-terminated by the kernel.
        let name = unsafe { CStr::from_ptr(bsd.pbi_name.as_ptr()) };
        let name = if name.to_bytes().is_empty() { unsafe { CStr::from_ptr(bsd.pbi_comm.as_ptr()) } } else { name };
        name.to_string_lossy().into_owned()
    };
    let mut info = ProcInfo { pid, name: kernel_name, cwd: cwd(pid), ..ProcInfo::default() };
    if let Some(buf) = procargs(pid, argmax) {
        parse_procargs(&buf, &mut info);
    }
    info
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

    #[test]
    fn procargs_layout_is_parsed() {
        let mut buf = 2i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"./.venv/bin/python\0\0\0\0python\0app.py\0HOME=/h\0VIRTUAL_ENV=/p/.venv\0\0");
        let mut info = ProcInfo::default();
        parse_procargs(&buf, &mut info);
        assert_eq!(info.exe, Some(PathBuf::from("./.venv/bin/python")));
        assert_eq!(info.name, "python");
        assert_eq!(info.args, vec![OsString::from("python"), OsString::from("app.py")]);
        assert_eq!(info.env_prefixes, vec![PathBuf::from("/p/.venv")]);
    }

    #[test]
    fn this_process_is_listed_with_its_executable() {
        let me = current_user_processes().into_iter().find(|p| p.pid == std::process::id() as i32).expect("self");
        assert!(me.exe.is_some() && me.cwd.is_some(), "{me:?}");
    }
}
