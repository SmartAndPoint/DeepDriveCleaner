//! Processes of the current user, for the executor's in-use and owner checks.

use std::ffi::{CStr, OsStr, OsString};
use std::mem::{MaybeUninit, size_of};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Component, Path, PathBuf};

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
    /// Paths of the files it has open, as the kernel reports them (see
    /// [`open_files`]); empty for this process, its children, or when unreadable.
    pub open_files: Vec<PathBuf>,
}

impl ProcInfo {
    /// The app this process belongs to, as its bundle name (`Cursor` for
    /// `Cursor.app`); see [`app_bundle`]. A relative executable path is joined
    /// with the working directory; without either, `None`.
    pub fn app_bundle(&self) -> Option<String> {
        let exe = self.exe.as_deref()?;
        if exe.is_absolute() { app_bundle(exe) } else { app_bundle(&self.cwd.as_deref()?.join(exe)) }
    }
}

/// The outermost `<Name>.app` in an executable path, when the executable lies in
/// that bundle's `Contents/MacOS` or `Contents/Frameworks`. Every app keeps its main
/// binary in `Contents/MacOS`; Electron and Chrome helpers live under
/// `Contents/Frameworks`. Tools shipped inside an app but run on their own (Xcode's
/// `clangd` under `Contents/Developer`) and system XPC services are not the app.
pub fn app_bundle(exe: &Path) -> Option<String> {
    let mut parts = exe.components().filter_map(|c| match c {
        Component::Normal(s) => Some(s),
        _ => None,
    });
    let name = parts.find_map(|c| c.to_str()?.strip_suffix(".app"))?;
    let inside = (parts.next()?, parts.next()?);
    (!name.is_empty() && inside.0 == "Contents" && (inside.1 == "MacOS" || inside.1 == "Frameworks"))
        .then(|| name.to_owned())
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

/// A file a process has open, as the kernel describes it: no `stat` of the
/// path is needed, so nothing here can make macOS ask (story "Coding agents
/// in the Whole disk view").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenVnode {
    /// As `MetadataExt::dev` gives it, so the pair compares with a `stat`.
    pub dev: u64,
    pub ino: u64,
    pub path: Option<PathBuf>,
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

/// When a process started and whose it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStart {
    pub uid: u32,
    /// Seconds since the epoch.
    pub sec: u64,
    pub usec: u64,
}

/// The start of `pid`, of any user; `None` when there is no such process.
pub fn process_start(pid: i32) -> Option<ProcStart> {
    let bsd = bsd_info(pid)?;
    Some(ProcStart { uid: bsd.pbi_uid, sec: bsd.pbi_start_tvsec, usec: bsd.pbi_start_tvusec })
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
    fn app_bundle_is_the_outermost_app_running_from_macos_or_frameworks() {
        let bundle = |exe: &str| app_bundle(Path::new(exe));
        assert_eq!(bundle("/Applications/Cursor.app/Contents/MacOS/Cursor").as_deref(), Some("Cursor"));
        assert_eq!(bundle("/Applications/Windsurf.app/Contents/MacOS/Electron").as_deref(), Some("Windsurf"));
        assert_eq!(
            bundle("/Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper (Renderer).app/Contents/MacOS/Code Helper (Renderer)")
                .as_deref(),
            Some("Visual Studio Code")
        );
        assert_eq!(
            bundle(
                "/Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Framework.framework/Versions/141.0/Helpers/Google Chrome Helper (Renderer).app/Contents/MacOS/Google Chrome Helper (Renderer)"
            )
            .as_deref(),
            Some("Google Chrome")
        );
        assert_eq!(
            bundle("/Applications/Firefox.app/Contents/MacOS/plugin-container.app/Contents/MacOS/plugin-container")
                .as_deref(),
            Some("Firefox")
        );
        assert_eq!(
            bundle("/Applications/Adobe Photoshop 2025/Adobe Photoshop 2025.app/Contents/MacOS/Adobe Photoshop 2025")
                .as_deref(),
            Some("Adobe Photoshop 2025")
        );
        assert_eq!(
            bundle("/private/var/folders/x/T/AppTranslocation/0A1B/d/Cursor.app/Contents/MacOS/Cursor").as_deref(),
            Some("Cursor")
        );
        assert_eq!(
            bundle("/Applications/Xcode.app/Contents/Developer/Toolchains/XcodeDefault.xctoolchain/usr/bin/clangd"),
            None
        );
        assert_eq!(
            bundle(
                "/System/Library/PrivateFrameworks/TextInputUIMacHelper.framework/Versions/A/XPCServices/CursorUIViewService.xpc/Contents/MacOS/CursorUIViewService"
            ),
            None
        );
        assert_eq!(bundle("/Applications/Claude.app/Contents/Helpers/disclaimer"), None);
        assert_eq!(bundle("/usr/bin/python3"), None);
    }

    #[test]
    fn a_relative_executable_needs_the_working_directory() {
        let relative =
            ProcInfo { exe: Some(PathBuf::from("./Cursor.app/Contents/MacOS/Cursor")), ..ProcInfo::default() };
        assert_eq!(relative.app_bundle(), None);
        let with_cwd = ProcInfo { cwd: Some(PathBuf::from("/Users/u/Applications")), ..relative };
        assert_eq!(with_cwd.app_bundle().as_deref(), Some("Cursor"));
        let named_only = ProcInfo { name: "Cursor".into(), ..ProcInfo::default() };
        assert_eq!(named_only.app_bundle(), None);
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
        let held = dir.path().canonicalize().unwrap().join("held.bin");
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
    fn this_process_started_before_now_as_this_user() {
        let start = process_start(std::process::id() as i32).unwrap();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(start.uid, uid());
        assert!(start.sec > 0 && start.sec <= now && start.usec < 1_000_000);
    }
}
