//! The data types the OS modules share (ADR "Platform seam: one platform
//! module per OS with declared capabilities", §1): defined once here, never
//! per OS. Per-OS values, such as errno numbers, are items of each OS
//! module (`platform::errno`), never here: `EDEADLK` is 11 on macOS and 35
//! on Linux.

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

// ---- Directory listing (`platform::bulk`, `scan::reader`) ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EntryKind {
    Dir,
    File,
    Symlink,
    Other,
}

/// One directory entry, never following symlinks.
#[derive(Debug, Clone)]
pub struct EntryMeta {
    pub name: OsString,
    pub kind: EntryKind,
    pub dev: u64,
    pub ino: u64,
    pub nlink: u32,
    /// `st_flags` (`SF_DATALESS`, …).
    pub flags: u32,
    /// Allocated bytes on disk. Directories report 0 (as on APFS).
    pub alloc: u64,
    /// APFS private size: bytes freed immediately if this file alone were
    /// deleted. `None` when not requested or not supported.
    pub private: Option<u64>,
    /// Per-entry errno reported by the filesystem; other fields may be unset.
    pub error: Option<i32>,
    /// Modification time, Unix seconds. Only from a listing that asks for
    /// [`Times::Modified`] or [`Times::ModifiedAndAdded`].
    pub modified: Option<i64>,
    /// When the entry was added to this directory (`ATTR_CMN_ADDEDTIME`),
    /// Unix seconds. Only for [`Times::ModifiedAndAdded`], where the
    /// filesystem keeps it.
    pub added: Option<i64>,
}

/// The times a listing reads for each entry. Each one adds bytes to every
/// record, so a listing asks only for what it uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Times {
    #[default]
    None,
    /// [`EntryMeta::modified`]: a directory inside a claim, for the newest
    /// change in it (story "Review window: sort a group's rows by name,
    /// size or last change").
    Modified,
    /// [`EntryMeta::modified`] and [`EntryMeta::added`]: a user-data
    /// folder's own listing (story "User data: pick your own files for the
    /// Trash").
    ModifiedAndAdded,
}

/// Entries of one directory plus the identity of the directory itself.
#[derive(Debug, Clone)]
pub struct DirListing {
    pub dev: u64,
    pub ino: u64,
    /// The directory's own modification time, Unix seconds, from the stat
    /// the open makes anyway, whatever [`Times`] asked. `None` from a
    /// reader that cannot tell.
    pub modified: Option<i64>,
    pub entries: Vec<EntryMeta>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ReadErrorKind {
    PermissionDenied,
    NotFound,
    /// Evicted cloud item with materialization off (`EDEADLK` on macOS).
    Dataless,
    /// The path no longer is the directory the parent listed (symlink swap, replaced).
    Changed,
    /// A different filesystem is mounted here.
    OtherFilesystem,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadError {
    pub kind: ReadErrorKind,
    pub errno: Option<i32>,
}

impl ReadError {
    pub fn new(kind: ReadErrorKind, errno: Option<i32>) -> Self {
        Self { kind, errno }
    }

    /// The kind this platform gives `errno` (`platform::errno::read_error_kind`).
    pub fn from_errno(errno: i32) -> Self {
        Self { kind: super::errno::read_error_kind(errno), errno: Some(errno) }
    }

    /// From an `io::Error`; one without an OS error counts as `EIO`.
    pub fn from_io(err: &io::Error) -> Self {
        Self::from_errno(err.raw_os_error().unwrap_or(super::errno::EIO))
    }
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.errno {
            Some(errno) => write!(f, "{:?}: {}", self.kind, io::Error::from_raw_os_error(errno)),
            None => write!(f, "{:?}", self.kind),
        }
    }
}

impl std::error::Error for ReadError {}

// ---- Verified-parent operations (`platform::fsops`) ----

/// Identity and type of an entry, from `fstatat(.., AT_SYMLINK_NOFOLLOW)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryStat {
    pub dev: u64,
    pub ino: u64,
    pub is_dir: bool,
    pub is_file: bool,
    pub is_symlink: bool,
    /// `st_flags` (`SF_DATALESS`, …).
    pub flags: u32,
    /// Allocated bytes (`st_blocks * 512`).
    pub alloc: u64,
}

// ---- Volumes (`platform::statfs`) ----

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeStats {
    /// `apfs`, `hfs`, `smbfs`, …
    pub fs_type: String,
    pub mount_point: PathBuf,
    /// Bytes available to an unprivileged user.
    pub available_bytes: u64,
    pub total_bytes: u64,
    /// Free bytes, the root's reserve included (`f_bfree`): on APFS
    /// `total_bytes - free_bytes` is what this volume uses.
    pub free_bytes: u64,
    /// The device or source mounted here (`/dev/disk3s5`, `map auto_home`).
    pub mounted_from: String,
}

// ---- Processes (`platform::procs`) ----

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
    /// `procs::open_files`); empty for this process, its children, or when unreadable.
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

/// When a process started and whose it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStart {
    pub uid: u32,
    /// Seconds since the epoch.
    pub sec: u64,
    pub usec: u64,
}

// ---- Resource usage (`platform::usage`) ----

/// What one process used.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub user: Duration,
    pub system: Duration,
    pub max_rss_bytes: u64,
}

impl Usage {
    /// User plus system time.
    pub fn cpu(&self) -> Duration {
        self.user + self.system
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
