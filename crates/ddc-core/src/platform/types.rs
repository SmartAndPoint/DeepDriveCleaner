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

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ---- Redirected folders (`platform::folders`, `Env`) ----

/// Which variable a redirected folder came from (safety ADR §6, Windows,
/// redirected and system folders): an entry of the protected list below its
/// default place in the home is protected below that variable's value too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Redirect {
    /// `APPDATA`, by default `AppData\Roaming`.
    Roaming,
    /// `LOCALAPPDATA`, by default `AppData\Local`.
    Local,
}

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
    /// The owner's user id; `None` where the reader does not tell (macOS
    /// does not yet: safety ADR, amendment of 2026-10-31).
    pub uid: Option<u32>,
    /// `st_flags` (`SF_DATALESS`, …).
    pub flags: u32,
    /// Allocated bytes on disk. A directory's are its own blocks (none on
    /// APFS), which the walk counts from its listing's [`DirListing::alloc`]
    /// once it reads the directory, and from here only when it cannot.
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
    /// A mount the walk must not open, where the reader tells one apart
    /// from its folder by more than the device (Linux: safety ADR,
    /// amendment of 2026-11-01). `None` on macOS, whose volumes the device
    /// alone tells apart.
    pub boundary: Option<Boundary>,
}

/// Why an entry is the edge of the walk although its device may be its
/// folder's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    /// An automount point: opening it would mount it.
    Automount,
    /// Another mount: a bind mount of the same filesystem keeps its device.
    Mount,
    /// A btrfs snapshot (read-only, writable or received), or `.zfs` at a
    /// ZFS dataset's root: never entered (safety ADR, amendment of
    /// 2026-11-02, "btrfs and ZFS on Linux").
    Snapshot,
    /// A plain btrfs subvolume of this mount: entered and counted, and the
    /// finding that holds it, or is it, is never planned (same amendment).
    Subvolume,
    /// A ZFS child dataset of this mount, by mountinfo's line for the mount
    /// id it carries: entered beneath an anchor of its own, held on that
    /// mount, and kept from a plan as [`Boundary::Subvolume`] is (same
    /// amendment).
    Dataset(u64),
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
    /// The directory's own allocated bytes, `st_blocks * 512` from the same
    /// stat: what `du` counts for the folder itself. 0 on APFS, which gives a
    /// directory no blocks; a few KiB on ext4. Each folder counts once: from
    /// here when it is read, from its parent's entry when it cannot be.
    pub alloc: u64,
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
    /// A mount root, an automount point or another mount than its folder's
    /// (Linux; safety ADR §5, amendment of 2026-11-01). Always `false` on
    /// macOS and Windows, where `dev` tells a volume.
    pub mount_root: bool,
}

// ---- The Trash (`platform::trash`) ----

/// Where an item went: the Trash folder it was moved into and its name
/// there, where the Trash tells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashPlace {
    pub trash: PathBuf,
    pub name: OsString,
}

/// What a move to the Trash did, when it did something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrashDone {
    /// The item is in the Trash, at the place given when the Trash tells.
    Moved(Option<TrashPlace>),
    /// The item is gone from its folder and is not in the Recycle Bin: the
    /// shell destroyed it instead (safety ADR §5, the Recycle Bin on
    /// Windows). Never on macOS or Linux.
    RemovedNotRecycled,
}

/// One part of a move the Trash is asked about before it moves anything:
/// `name` in the verified folder every part of one ask shares, `bytes` its
/// size as measured for the clean.
#[derive(Debug, Clone, Copy)]
pub struct TrashAsk<'a> {
    pub name: &'a std::ffi::OsStr,
    pub path: &'a Path,
    pub bytes: u64,
    pub is_dir: bool,
}

/// Why the Recycle Bin refuses an item (safety ADR §5, the Recycle Bin on
/// Windows, planned 2026-11-04). Nothing moved; `--permanent` removes it
/// instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum RecycleBinCause {
    /// The item's volume has no Recycle Bin: not a local fixed drive, or
    /// the shell finds none there.
    NoBin,
    /// The item is larger than the bin's maximum size.
    TooLarge,
    /// The item fits the bin's maximum, not the room left in it: Windows
    /// would destroy the user's oldest recycled items to make room.
    BinFull,
    /// The volume's bin deletes at once (`NukeOnDelete`).
    NukeOnDelete,
    /// A policy turns the Recycle Bin off (`NoRecycleFiles`).
    NoRecycleFiles,
    /// The shell does not take the item's path (longer than `MAX_PATH`).
    PathRefused,
    /// A setting the checks need could not be read: refused, never guessed.
    SettingsUnreadable,
    /// The shell would delete or copy the item instead of recycling it.
    WouldDestroy,
}

impl RecycleBinCause {
    /// Every cause, in the order of the ADR.
    pub const ALL: [Self; 8] = [
        Self::NoBin,
        Self::TooLarge,
        Self::BinFull,
        Self::NukeOnDelete,
        Self::NoRecycleFiles,
        Self::PathRefused,
        Self::SettingsUnreadable,
        Self::WouldDestroy,
    ];
}

impl fmt::Display for RecycleBinCause {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoBin => "no Recycle Bin on the item's drive",
            Self::TooLarge => "larger than the Recycle Bin's maximum size",
            Self::BinFull => "the Recycle Bin is full; empty it",
            Self::NukeOnDelete => "the Recycle Bin of this drive deletes at once",
            Self::NoRecycleFiles => "a policy turns the Recycle Bin off",
            Self::PathRefused => "the path is too long for the Recycle Bin",
            Self::SettingsUnreadable => "the Recycle Bin's settings could not be read",
            Self::WouldDestroy => "the shell would delete it, not recycle it",
        })
    }
}

/// Why a move to the Trash did not happen. Nothing was moved either way.
#[derive(Debug)]
pub enum TrashError {
    /// The item's filesystem has no Trash ddc may use, or the move would
    /// cross into another (safety ADR §5, amendment of 2026-11-01).
    CrossesFilesystem,
    /// The Recycle Bin refuses the item (Windows only).
    RecycleBinRefuses(RecycleBinCause),
    /// Another process holds the item or a file below it, so the move was
    /// refused with the item whole (Windows only).
    HeldOpen,
    Other(io::Error),
}

impl fmt::Display for TrashError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CrossesFilesystem => f.write_str("no Trash on the item's filesystem"),
            Self::RecycleBinRefuses(cause) => cause.fmt(f),
            Self::HeldOpen => f.write_str("held open by another process"),
            Self::Other(e) => e.fmt(f),
        }
    }
}

/// The room left in each volume's Recycle Bin during one clean or one dry
/// run (safety ADR §5, the Recycle Bin on Windows, planned 2026-11-04):
/// read once per volume, at the first ask there, and taken from by each
/// ask that fits, all of its bytes or none. Nothing is given back, so a
/// dry run and its clean, asking in the same order, refuse the same items.
#[derive(Debug, Clone)]
pub struct BinBudget<K: Ord> {
    room: std::collections::BTreeMap<K, u64>,
}

impl<K: Ord> Default for BinBudget<K> {
    fn default() -> Self {
        Self { room: std::collections::BTreeMap::new() }
    }
}

impl<K: Ord> BinBudget<K> {
    /// Takes `bytes` from `volume`'s room, reading the room through `read`
    /// at the volume's first ask: [`RecycleBinCause::BinFull`] when they do
    /// not fit, and nothing is taken. A room that cannot be read refuses
    /// with `read`'s cause, and is read again at the next ask.
    pub fn take(
        &mut self,
        volume: K,
        bytes: u64,
        read: impl FnOnce() -> Result<u64, RecycleBinCause>,
    ) -> Result<(), RecycleBinCause> {
        let room = match self.room.entry(volume) {
            std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::btree_map::Entry::Vacant(e) => e.insert(read()?),
        };
        *room = room.checked_sub(bytes).ok_or(RecycleBinCause::BinFull)?;
        Ok(())
    }
}

impl From<io::Error> for TrashError {
    fn from(e: io::Error) -> Self {
        Self::Other(e)
    }
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
    /// The executable's path. On macOS the path passed to `execve`, before
    /// symlinks are resolved, and may be relative; on Linux the resolved
    /// path the kernel gives (`/proc/<pid>/exe`).
    pub exe: Option<PathBuf>,
    /// The executable after every symlink is resolved, as the kernel holds
    /// it: `proc_pidpath` on macOS, `/proc/<pid>/exe` on Linux. A process
    /// started through a link (Claude Code's `~/.local/bin/claude`) runs the
    /// file it points to; this is that file. `None` when unreadable.
    pub resolved_exe: Option<PathBuf>,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// `VIRTUAL_ENV` / `CONDA_PREFIX` from the environment, when readable.
    pub env_prefixes: Vec<PathBuf>,
    /// Paths of the files it has open, as the kernel reports them (see
    /// `procs::open_files`); empty for this process, its children, or when unreadable.
    pub open_files: Vec<PathBuf>,
    /// Some of the above could not be read: a non-dumpable process
    /// withholds them, and one that exits during the read leaves them empty
    /// (safety ADR, §5, amendment of 2026-10-31). The dry run and the clean
    /// report count such processes.
    pub withheld: bool,
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
    /// As `meta::stat` gives it, so the pair compares with a scan's: on
    /// Linux btrfs and ZFS the folded `f_fsid`, elsewhere `st_dev`.
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

/// Whose a scan root is, for the owner check of an elevated session
/// (`procs::root_owner`; story "Windows scanner and rule catalog", step 7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootOwner {
    /// The user's, or nobody's in particular (no owner kept, or no session
    /// with more rights than the user).
    Own,
    /// Another account's: its SID, as `S-1-5-18`.
    Other(String),
}

/// Why this process's token cannot be used for the owner check: it cannot
/// be read, or it still holds a privilege that passes every ACL. The
/// payload of `procs::root_owner`'s error, so a refusal can tell it from an
/// owner that cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenUnusable(pub String);

impl fmt::Display for TokenUnusable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TokenUnusable {}

/// A folder another process holds, at one of the two stages of the
/// tombstone rename (safety ADR §5, amendment of 2026-11-02): opening it
/// with `DELETE` met a sharing violation, a process working inside it; or
/// the open succeeded and the rename was denied, a file below it open, still
/// so after the bounded retry. The payload of `DirFd::rename_child`'s error
/// on Windows only, recognised by [`HeldOpen::is`]; every other OS and every
/// other stage returns its plain error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldOpen {
    /// Opening the folder with `DELETE` failed with a sharing violation.
    AtOpen,
    /// The open succeeded; the rename was denied at every attempt.
    AtRename,
}

impl HeldOpen {
    /// Whether `e` carries a [`HeldOpen`].
    #[must_use]
    pub fn is(e: &io::Error) -> bool {
        e.get_ref().is_some_and(|p| p.is::<Self>())
    }
}

impl fmt::Display for HeldOpen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::AtOpen => "held open: a process works inside the folder (sharing violation, 32)",
            Self::AtRename => "held open: a file below the folder is open (access denied at the rename, 5)",
        })
    }
}

impl std::error::Error for HeldOpen {}

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

/// What `meta::lstat` and `meta::stat` give for one path: its identity
/// (`dev`, `ino`, as a directory listing gives them), its hard links and the
/// bytes allocated on disk, which std's portable `Metadata` does not carry
/// on every OS, beside that `Metadata` for the rest (times, length, kind).
#[derive(Debug, Clone)]
pub struct FileStat {
    pub dev: u64,
    pub ino: u64,
    pub nlink: u64,
    pub alloc: u64,
    pub md: std::fs::Metadata,
}

impl FileStat {
    /// `(dev, ino)`.
    pub fn id(&self) -> (u64, u64) {
        (self.dev, self.ino)
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

    #[test]
    fn a_bin_budget_reads_each_volume_once_and_takes_all_or_nothing() {
        let reads = std::cell::Cell::new(0);
        let room = |n: u64| {
            let reads = &reads;
            move || {
                reads.set(reads.get() + 1);
                Ok(n)
            }
        };
        let mut budget = BinBudget::<u8>::default();
        assert_eq!(budget.take(1, 40, room(100)), Ok(()));
        assert_eq!(budget.take(1, 40, room(999)), Ok(()), "the room is read at the first ask only");
        assert_eq!(budget.take(1, 40, room(999)), Err(RecycleBinCause::BinFull), "20 left");
        assert_eq!(budget.take(1, 20, room(999)), Ok(()), "a refused ask took nothing");
        assert_eq!(budget.take(1, 1, room(999)), Err(RecycleBinCause::BinFull));
        assert_eq!(budget.take(2, 50, room(50)), Ok(()), "another volume has its own room");
        assert_eq!(reads.get(), 2);
    }

    #[test]
    fn a_bin_room_that_cannot_be_read_refuses_and_is_read_again() {
        let mut budget = BinBudget::<u8>::default();
        assert_eq!(
            budget.take(1, 1, || Err(RecycleBinCause::SettingsUnreadable)),
            Err(RecycleBinCause::SettingsUnreadable)
        );
        assert_eq!(budget.take(1, 1, || Ok(1)), Ok(()));
    }

    #[test]
    fn every_recycle_bin_cause_is_listed_once_and_named_in_kebab_case() {
        let names: Vec<String> = RecycleBinCause::ALL
            .iter()
            .map(|c| serde_json::to_value(c).unwrap().as_str().unwrap().to_owned())
            .collect();
        assert_eq!(
            names,
            [
                "no-bin",
                "too-large",
                "bin-full",
                "nuke-on-delete",
                "no-recycle-files",
                "path-refused",
                "settings-unreadable",
                "would-destroy"
            ]
        );
    }
}
