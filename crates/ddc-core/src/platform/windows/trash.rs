//! The Windows Recycle Bin (safety ADR §5, the Recycle Bin on Windows,
//! planned 2026-11-04 and settled at step 10a's planning; story "Windows
//! scanner and rule catalog", step 10b): the shell's `IFileOperation`,
//! called directly, one item per operation, on a short-lived thread of its
//! own single-threaded apartment, so the console handler and the cancel
//! event's watcher never touch COM. This is the one file that uses the
//! `windows` crate, for its COM interfaces (seam ADR §2, amended
//! 2026-11-04); every other call goes through `windows-sys`.
//!
//! The shell, called with no UI, may destroy an item the bin cannot take
//! instead of failing. A progress sink reads the shell's own answer for
//! the item: `PostDeleteItem` gives its result and the item now in the bin,
//! none when the shell destroyed it.
//!
//! Before a move ([`admit`]), every check is static, so the dry run asks
//! the same: the volume is a local fixed drive, not FAT or exFAT, with a
//! Recycle Bin; neither `NukeOnDelete` nor the policy `NoRecycleFiles` is
//! set; the item fits the bin's maximum (`MaxCapacity`) and the room the
//! run has left there (a [`BinBudget`] per volume, read once through
//! `SHQueryRecycleBinW`); the shell takes the path. A value that cannot be
//! read refuses as `settings-unreadable`: `admit` has no other error.
//!
//! The move ([`trash_item`]): the item checked unchanged through its
//! verified folder, its plain path bound to that item by opening it the
//! way the shell will, then the shell's move. After it, without listing the
//! bin: the source is gone from its folder and the item the shell reports
//! in the bin has its file id, on its volume, under `$Recycle.Bin`; then it
//! is `trashed`. Gone with no item in the bin is `removed-not-recycled`;
//! whole after a refusal is that refusal's cause or `held-open`; anything
//! else is a failure, the bin's path named where the shell gave one.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use windows::Win32::Foundation::E_ABORT;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::{
    COPYENGINE_E_ACCESS_DENIED_SRC, COPYENGINE_E_RECYCLE_BIN_NOT_FOUND, COPYENGINE_E_RECYCLE_FORCE_NUKE,
    COPYENGINE_E_RECYCLE_PATH_TOO_LONG, COPYENGINE_E_RECYCLE_SIZE_TOO_BIG, COPYENGINE_E_SHARING_VIOLATION_DEST,
    COPYENGINE_E_SHARING_VIOLATION_SRC, COPYENGINE_S_NOT_HANDLED, FILEOPERATION_FLAGS, FOF_ALLOWUNDO, FOF_NO_UI,
    FOFX_NOCOPYHOOKS, FOFX_RECYCLEONDELETE, FileOperation, IFileOperation, IFileOperationProgressSink,
    IFileOperationProgressSink_Impl, IShellItem, SHCreateItemFromParsingName, SIGDN_FILESYSPATH,
};
use windows::core::{HRESULT, PCWSTR, Ref, implement};
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_NO_RECALL, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
    FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDriveTypeW,
    GetFinalPathNameByHandleW, OPEN_EXISTING, VOLUME_NAME_GUID,
};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW,
};
use windows_sys::Win32::UI::Shell::{SHQUERYRBINFO, SHQueryRecycleBinW};

use super::fsops::DirFd;
use crate::platform::types::{BinBudget, RecycleBinCause, TrashAsk, TrashDone, TrashError, TrashPlace};

/// The operation's flags: no UI, recycle, and no copy hook (a third-party
/// extension that could veto or slow the move). Not `FOF_WANTNUKEWARNING`:
/// with it the operation waits on a confirmation no one sees (lab L0).
const FLAGS: FILEOPERATION_FLAGS =
    FILEOPERATION_FLAGS(FOF_NO_UI.0 | FOF_ALLOWUNDO.0 | FOFX_RECYCLEONDELETE.0 | FOFX_NOCOPYHOOKS.0);

/// `GetDriveTypeW`'s answer for a local fixed drive (WinBase.h).
const DRIVE_FIXED: u32 = 3;
/// Where Explorer keeps each volume's Recycle Bin settings, under HKCU.
const BIT_BUCKET: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\BitBucket\Volume\";
/// Where the Recycle Bin's policies live, under HKCU and HKLM.
const POLICIES: &str = r"Software\Microsoft\Windows\CurrentVersion\Policies\Explorer";

fn refused(cause: RecycleBinCause) -> TrashError {
    TrashError::RecycleBinRefuses(cause)
}

/// The Recycle Bin's policies, read once per run.
#[derive(Debug, Clone, Copy)]
struct Policy {
    /// `NoRecycleFiles`, in HKCU or HKLM.
    no_recycle: bool,
    /// `RecycleBinSize` is set: its meaning waits for the lab, so it refuses.
    bin_size: bool,
}

/// One volume's facts, read at its first ask.
#[derive(Debug, Clone)]
struct Facts {
    /// Where the volume is mounted, for `SHQueryRecycleBinW`.
    mount: PathBuf,
    drive: u32,
    fat: bool,
    /// `MaxCapacity`, in MiB; `None` where the bin was never used.
    max_mb: Option<u32>,
    nuke: bool,
}

/// What one clean or one dry run keeps between its asks: the policies, each
/// volume's facts, and the room left in each volume's bin, keyed by the
/// volume's GUID path.
#[derive(Debug, Default)]
pub struct Budget {
    policy: Option<Policy>,
    facts: BTreeMap<String, Facts>,
    room: BinBudget<String>,
}

/// The check before a move, and the dry run's answer: would the Recycle Bin
/// take `asks`, every part of one move, all in the folder `parent`? Their
/// bytes are taken from the volume's room when it would. Every error is a
/// refusal; one that cannot be read is `settings-unreadable`.
pub fn admit(budget: &mut Budget, parent: &DirFd, asks: &[TrashAsk<'_>]) -> Result<(), TrashError> {
    let Some(first) = asks.first() else { return Ok(()) };
    let guid = volume_guid(parent.handle()).map_err(|_| refused(RecycleBinCause::SettingsUnreadable))?;
    let policy = match budget.policy {
        Some(policy) => policy,
        None => *budget.policy.insert(read_policy().ok_or(refused(RecycleBinCause::SettingsUnreadable))?),
    };
    if !budget.facts.contains_key(&guid) {
        let facts = read_facts(&guid, first.path).ok_or(refused(RecycleBinCause::SettingsUnreadable))?;
        budget.facts.insert(guid.clone(), facts);
    }
    let facts = &budget.facts[&guid];
    let max = refusal(policy, facts, asks)?;
    let mut total = 0u64;
    for ask in asks {
        let bytes = ask.bytes.max(shell_counts(parent, ask, max)?);
        if bytes > max {
            return Err(refused(RecycleBinCause::TooLarge));
        }
        total = total.saturating_add(bytes);
    }
    let mount = facts.mount.clone();
    budget.room.take(guid, total, || room(&mount, max)).map_err(refused)
}

/// What the shell counts for `ask` in `parent`, which can be far more than
/// the ask's allocated bytes (settled at 10b's lab), up to past `max`. Every
/// error of the walk is a refusal, `held-open`: an `Other` would be met by
/// moving the item unchecked.
fn shell_counts(parent: &DirFd, ask: &TrashAsk<'_>, max: u64) -> Result<u64, TrashError> {
    super::bulk::recycled_size(parent, ask.name, max).map_err(|_| TrashError::HeldOpen)
}

/// The static checks, the first that fails named: the bin's maximum, in
/// bytes, when every part passes.
fn refusal(policy: Policy, facts: &Facts, asks: &[TrashAsk<'_>]) -> Result<u64, TrashError> {
    if policy.no_recycle {
        return Err(refused(RecycleBinCause::NoRecycleFiles));
    }
    if facts.drive != DRIVE_FIXED {
        return Err(refused(RecycleBinCause::NoBin));
    }
    // No file id to check the move by (story step 6b): no Trash ddc may use.
    if facts.fat {
        return Err(TrashError::CrossesFilesystem);
    }
    if facts.nuke {
        return Err(refused(RecycleBinCause::NukeOnDelete));
    }
    // Windows' default for a bin never used waits for the lab (L2).
    let (Some(max_mb), false) = (facts.max_mb, policy.bin_size) else {
        return Err(refused(RecycleBinCause::SettingsUnreadable));
    };
    let max = u64::from(max_mb) << 20;
    for ask in asks {
        if shell_path(ask.path).is_none() {
            return Err(refused(RecycleBinCause::PathRefused));
        }
        if ask.bytes > max {
            return Err(refused(RecycleBinCause::TooLarge));
        }
    }
    Ok(max)
}

/// The room left in the bin at `mount`: its maximum less what it holds.
fn room(mount: &Path, max: u64) -> Result<u64, RecycleBinCause> {
    let root = nul_terminated(mount).ok_or(RecycleBinCause::SettingsUnreadable)?;
    let mut info = SHQUERYRBINFO {
        cbSize: u32::try_from(size_of::<SHQUERYRBINFO>()).expect("a small struct"),
        i64Size: 0,
        i64NumItems: 0,
    };
    // SAFETY: a NUL-terminated root path and a struct with its size set,
    // both live for the call.
    if unsafe { SHQueryRecycleBinW(root.as_ptr(), &mut info) } < 0 {
        return Err(RecycleBinCause::SettingsUnreadable);
    }
    let held = u64::try_from(info.i64Size).map_err(|_| RecycleBinCause::SettingsUnreadable)?;
    Ok(max.saturating_sub(held))
}

fn read_policy() -> Option<Policy> {
    let mut no_recycle = false;
    let mut bin_size = false;
    for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        no_recycle |= dword(hive, POLICIES, "NoRecycleFiles")?.is_some_and(|v| v != 0);
        bin_size |= dword(hive, POLICIES, "RecycleBinSize")?.is_some();
    }
    Some(Policy { no_recycle, bin_size })
}

/// The facts of the volume `guid` (`\\?\Volume{…}\`), which `path` must be
/// on; `None` when one cannot be read.
fn read_facts(guid: &str, path: &Path) -> Option<Facts> {
    let st = super::statfs::statfs(path).ok()?;
    if !st.mounted_from.eq_ignore_ascii_case(guid) {
        return None;
    }
    let root = nul_terminated(&st.mount_point)?;
    // SAFETY: a NUL-terminated root path, live for the call.
    let drive = unsafe { GetDriveTypeW(root.as_ptr()) };
    let braces = guid.strip_prefix(r"\\?\Volume")?.trim_end_matches('\\');
    let key = format!("{BIT_BUCKET}{braces}");
    let max_mb = dword(HKEY_CURRENT_USER, &key, "MaxCapacity")?;
    let nuke = dword(HKEY_CURRENT_USER, &key, "NukeOnDelete")?.is_some_and(|v| v != 0);
    Some(Facts { mount: st.mount_point, drive, fat: super::fsops::is_fat(&st.fs_type), max_mb, nuke })
}

/// A `REG_DWORD` value: `Some(None)` when the key or the value is absent,
/// `None` when it cannot be read (another type, a denial).
fn dword(hive: HKEY, key: &str, value: &str) -> Option<Option<u32>> {
    let key: Vec<u16> = key.encode_utf16().chain([0]).collect();
    let value: Vec<u16> = value.encode_utf16().chain([0]).collect();
    let mut data = 0u32;
    let mut size = u32::try_from(size_of::<u32>()).expect("four bytes");
    // SAFETY: NUL-terminated key and value names and a four-byte output
    // buffer with its size, all live for the call; the type is not asked.
    let status = unsafe {
        RegGetValueW(
            hive,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&raw mut data).cast(),
            &mut size,
        )
    };
    match status {
        0 => Some(Some(data)),
        ERROR_FILE_NOT_FOUND => Some(None),
        _ => None,
    }
}

/// `path` as UTF-16 with its NUL; `None` for a path holding a NUL.
fn nul_terminated(path: &Path) -> Option<Vec<u16>> {
    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    (!wide.contains(&0)).then(|| wide.into_iter().chain([0]).collect())
}

/// The GUID path of the volume `handle` is on, `\\?\Volume{…}\`.
fn volume_guid(handle: &OwnedHandle) -> io::Result<String> {
    let mut buffer = vec![0u16; 512];
    loop {
        let capacity = u32::try_from(buffer.len()).map_err(|_| io::Error::from_raw_os_error(206))?;
        // SAFETY: the handle is open for the call; the buffer is writable
        // for `capacity` units.
        let n = unsafe {
            GetFinalPathNameByHandleW(
                handle.as_raw_handle(),
                buffer.as_mut_ptr(),
                capacity,
                FILE_NAME_NORMALIZED | VOLUME_NAME_GUID,
            )
        };
        let n = usize::try_from(n).expect("u32 fits usize");
        if n == 0 {
            return Err(io::Error::last_os_error());
        }
        if n < buffer.len() {
            let path = String::from_utf16(&buffer[..n]).map_err(|_| io::Error::from_raw_os_error(87))?;
            let end = path.find("}\\").ok_or_else(|| io::Error::from_raw_os_error(87))? + 2;
            return Ok(path[..end].to_owned());
        }
        buffer.resize(n.max(buffer.len() + 1), 0);
    }
}

/// Opens `plain` as the shell will: a plain path, Win32's parsing, no link
/// followed and no placeholder recalled, sharing everything, reading only
/// attributes.
fn open_plain(plain: &Path) -> io::Result<OwnedHandle> {
    let name = nul_terminated(plain).ok_or_else(|| io::Error::from_raw_os_error(123))?;
    // SAFETY: a NUL-terminated path, live for the call; the security
    // attributes and template are null, which CreateFileW allows.
    let raw = unsafe {
        CreateFileW(
            name.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_NO_RECALL | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a handle CreateFileW just returned, owned by nobody else.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

fn id_at(plain: &Path) -> io::Result<(u64, u64)> {
    let handle = open_plain(plain)?;
    super::meta::id_of(&super::meta::info::<FILE_ID_INFO>(&handle)?)
}

/// What the source is after the shell's move.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Gone,
    Whole,
    /// Another item has its name now, or it could not be read.
    Changed,
}

/// What the shell put in the bin, as the after-check found it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Bin {
    /// Nothing reported.
    None,
    /// The item, by its file id, on its volume, under `$Recycle.Bin`.
    Ours(PathBuf),
    /// Something else, or something that could not be checked.
    Other(PathBuf),
}

/// Moves `name`, in the verified folder `parent` (at `path`), to the
/// Recycle Bin. `id` is the identity the guard checked: the item must have
/// it before the move and in the bin after it.
pub fn trash_item(parent: &DirFd, name: &OsStr, path: &Path, id: (u64, u64)) -> Result<TrashDone, TrashError> {
    let unchanged = |what: &str| TrashError::Other(io::Error::other(format!("{}: {what}", path.display())));
    let st = parent.stat_child(name)?;
    if st.is_symlink || (st.dev, st.ino) != id {
        return Err(unchanged("changed before the move"));
    }
    let plain = shell_path(path).ok_or(refused(RecycleBinCause::PathRefused))?;
    // The shell names the item by its plain path: that path must reach this
    // very item and this very folder. The handles close before the move.
    let folder = plain.parent().ok_or_else(|| unchanged("no folder"))?;
    if id_at(&plain).ok() != Some(id) || id_at(folder).ok() != Some(parent.identity()?) {
        return Err(unchanged("its plain path names another item"));
    }
    let guid = volume_guid(parent.handle())?;
    let log = recycle(&plain, FLAGS, Veto::WouldDestroy)?;
    let source = match parent.stat_child(name) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Source::Gone,
        Ok(st) if !st.is_symlink && (st.dev, st.ino) == id => Source::Whole,
        _ => Source::Changed,
    };
    let post = ours(&log, &plain);
    let bin = match post.and_then(|p| p.new.as_deref()) {
        None => Bin::None,
        Some(new) if in_bin(new, id, &guid) => Bin::Ours(new.to_path_buf()),
        Some(new) => Bin::Other(new.to_path_buf()),
    };
    classify(&log, post, source, &bin, path)
}

/// The `PostDeleteItem` of the item itself: the one naming `plain`, or the
/// only one.
fn ours<'a>(log: &'a ShellLog, plain: &Path) -> Option<&'a Post> {
    let key = super::paths::key(plain);
    log.post
        .iter()
        .find(|p| p.item.as_deref().is_some_and(|i| super::paths::key(i) == key))
        .or_else(|| if log.post.len() == 1 { log.post.first() } else { None })
}

/// Whether `new` is the item: its file id, on the volume `guid`, in a
/// folder of that volume's `$Recycle.Bin`.
fn in_bin(new: &Path, id: (u64, u64), guid: &str) -> bool {
    let under_bin = new
        .parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .is_some_and(|n| n.eq_ignore_ascii_case("$Recycle.Bin"));
    let Ok(handle) = super::meta::open_with(new, FILE_READ_ATTRIBUTES, FILE_FLAG_OPEN_REPARSE_POINT) else {
        return false;
    };
    let same = super::meta::info::<FILE_ID_INFO>(&handle).and_then(|i| super::meta::id_of(&i)).ok() == Some(id);
    under_bin && same && volume_guid(&handle).is_ok_and(|g| g.eq_ignore_ascii_case(guid))
}

/// The shell's result and the after-check, as one answer (safety ADR §5,
/// the Recycle Bin on Windows, "the after-check").
fn classify(
    log: &ShellLog,
    post: Option<&Post>,
    source: Source,
    bin: &Bin,
    path: &Path,
) -> Result<TrashDone, TrashError> {
    let failed = |what: String| Err(TrashError::Other(io::Error::other(format!("{}: {what}", path.display()))));
    let hr = post.map(|p| p.hr).or(log.performed).unwrap_or(HRESULT(0));
    // A child deleted alone would be a folder partly moved; one the shell
    // left alone (`COPYENGINE_S_NOT_HANDLED`, after a veto) is not.
    let partly = log
        .post
        .iter()
        .any(|p| post.is_none_or(|q| !std::ptr::eq(p, q)) && p.hr.is_ok() && p.hr != COPYENGINE_S_NOT_HANDLED);
    match (source, bin) {
        (Source::Whole, _) if partly => failed("the Recycle Bin took part of it".to_owned()),
        (Source::Whole, Bin::None) => {
            if let Some(setup) = log.setup {
                return failed(format!("the Recycle Bin could not be asked ({:#010x})", setup.0));
            }
            // A folder over the maximum: the shell takes the folder to the
            // bin, then would destroy its children one by one (10b's lab).
            if log.vetoed && log.pre.first().is_some_and(|(_, flags)| asks_recycle(*flags)) {
                return Err(refused(RecycleBinCause::TooLarge));
            }
            if log.vetoed || hr == COPYENGINE_E_RECYCLE_FORCE_NUKE {
                return Err(refused(RecycleBinCause::WouldDestroy));
            }
            match hr {
                h if h == COPYENGINE_E_RECYCLE_SIZE_TOO_BIG => Err(refused(RecycleBinCause::TooLarge)),
                h if h == COPYENGINE_E_RECYCLE_PATH_TOO_LONG => Err(refused(RecycleBinCause::PathRefused)),
                h if h == COPYENGINE_E_RECYCLE_BIN_NOT_FOUND => Err(refused(RecycleBinCause::NoBin)),
                h if held_open(h) => Err(TrashError::HeldOpen),
                h if h.is_ok() => failed("the shell said done, and it is still there".to_owned()),
                h => failed(format!("the Recycle Bin refused it ({:#010x})", h.0)),
            }
        }
        (Source::Gone, Bin::Ours(new)) if hr.is_ok() => {
            let place = new.parent().zip(new.file_name());
            Ok(TrashDone::Moved(
                place.map(|(trash, name)| TrashPlace { trash: trash.to_path_buf(), name: name.to_owned() }),
            ))
        }
        (Source::Gone, Bin::None) if post.is_some() => Ok(TrashDone::RemovedNotRecycled),
        (Source::Gone, Bin::None) => failed("gone, and the shell did not say where".to_owned()),
        (_, Bin::Other(new) | Bin::Ours(new)) => {
            failed(format!("the shell answered {:#010x}; the Recycle Bin holds {}", hr.0, new.display()))
        }
        (Source::Changed, Bin::None) => failed("another item has its name after the move".to_owned()),
    }
}

/// Another program holds the item or a file below it: a sharing violation
/// or a denial on the source (lab experiment L1, 2026-11-04: a process
/// working inside gives `SHARING_VIOLATION_SRC`, an open file below or a
/// running program inside `SHARING_VIOLATION_DEST`, an open file shared
/// for delete `ACCESS_DENIED_SRC`), or the Win32 32 and 5.
fn held_open(hr: HRESULT) -> bool {
    [
        COPYENGINE_E_SHARING_VIOLATION_SRC,
        COPYENGINE_E_SHARING_VIOLATION_DEST,
        COPYENGINE_E_ACCESS_DENIED_SRC,
        HRESULT::from_win32(32),
        HRESULT::from_win32(5),
    ]
    .contains(&hr)
}

/// The longest path the shell takes, in UTF-16 units, without its NUL
/// (`MAX_PATH` with it).
const SHELL_PATH_MAX: usize = 259;

/// The plain path the shell is given for `path`, or `None` when the shell
/// would refuse it or read it as another item (`path-refused`): a name the
/// plain form does not keep (Win32 strips a trailing dot or space, and
/// `NUL` or `com1.txt` is a device), or a path of `MAX_PATH` characters or
/// more.
fn shell_path(path: &Path) -> Option<PathBuf> {
    let plain = super::paths::simplified(path.to_path_buf());
    let verbatim = plain.as_os_str().encode_wide().take(4).eq(r"\\?\".encode_utf16());
    let names = plain.components().all(|c| match c {
        std::path::Component::Normal(name) => super::paths::plainly_spelled(name),
        _ => true,
    });
    (!verbatim && names && plain.as_os_str().encode_wide().count() <= SHELL_PATH_MAX).then_some(plain)
}

/// One `PostDeleteItem` the shell reported: the item it was about, its
/// result, and the item it put in the bin, none when it put none there.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Post {
    item: Option<PathBuf>,
    hr: HRESULT,
    new: Option<PathBuf>,
}

/// What the shell said of one operation, as plain data: no COM object
/// leaves its thread.
#[derive(Debug, Default, Clone)]
struct ShellLog {
    /// `PerformOperations`' own result; `None` when the operation never got
    /// that far (an item or an interface that could not be made), with
    /// `setup` saying why.
    performed: Option<HRESULT>,
    setup: Option<HRESULT>,
    /// `GetAnyOperationsAborted`.
    aborted: bool,
    /// The sink refused the move at `PreDeleteItem`.
    vetoed: bool,
    /// Each `PreDeleteItem`: the item and its `TRANSFER_SOURCE_FLAGS`.
    pre: Vec<(Option<PathBuf>, u32)>,
    post: Vec<Post>,
}

/// When the sink refuses a move at `PreDeleteItem`.
#[derive(Debug, Clone, Copy)]
enum Veto {
    /// When the shell was not asked to recycle, or would copy and delete:
    /// either destroys the item (safety ADR, `would-destroy`).
    WouldDestroy,
    /// Always: the lab's check that a veto leaves the source whole.
    #[cfg_attr(not(test), allow(dead_code))]
    Always,
}

/// `TSF_DELETE_RECYCLE_IF_POSSIBLE` and `TSF_MOVE_AS_COPY_DELETE`, as the
/// sink receives them in `dwflags`.
const RECYCLE_IF_POSSIBLE: u32 = windows::Win32::UI::Shell::TSF_DELETE_RECYCLE_IF_POSSIBLE.0 as u32;
const MOVE_AS_COPY_DELETE: u32 = windows::Win32::UI::Shell::TSF_MOVE_AS_COPY_DELETE.0 as u32;

/// The shell would recycle the item `PreDeleteItem` names, by a move.
fn asks_recycle(flags: u32) -> bool {
    flags & RECYCLE_IF_POSSIBLE != 0 && flags & MOVE_AS_COPY_DELETE == 0
}

#[implement(IFileOperationProgressSink)]
struct Sink {
    log: Rc<RefCell<ShellLog>>,
    veto: Veto,
}

#[allow(non_snake_case)] // the interface's own names
impl IFileOperationProgressSink_Impl for Sink_Impl {
    fn StartOperations(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn FinishOperations(&self, _: HRESULT) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreRenameItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostRenameItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreMoveItem(&self, _: u32, _: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostMoveItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreCopyItem(&self, _: u32, _: Ref<IShellItem>, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostCopyItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn PreDeleteItem(&self, flags: u32, item: Ref<IShellItem>) -> windows::core::Result<()> {
        let mut log = self.log.borrow_mut();
        log.pre.push((item.as_ref().and_then(path_of), flags));
        let refuse = match self.veto {
            Veto::Always => true,
            Veto::WouldDestroy => !asks_recycle(flags),
        };
        if refuse {
            log.vetoed = true;
            return Err(E_ABORT.into());
        }
        Ok(())
    }
    fn PostDeleteItem(
        &self,
        _: u32,
        item: Ref<IShellItem>,
        hr: HRESULT,
        new: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        let post = Post { item: item.as_ref().and_then(path_of), hr, new: new.as_ref().and_then(path_of) };
        self.log.borrow_mut().post.push(post);
        Ok(())
    }
    fn PreNewItem(&self, _: u32, _: Ref<IShellItem>, _: &PCWSTR) -> windows::core::Result<()> {
        Ok(())
    }
    fn PostNewItem(
        &self,
        _: u32,
        _: Ref<IShellItem>,
        _: &PCWSTR,
        _: &PCWSTR,
        _: u32,
        _: HRESULT,
        _: Ref<IShellItem>,
    ) -> windows::core::Result<()> {
        Ok(())
    }
    fn UpdateProgress(&self, _: u32, _: u32) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResetTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn PauseTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
    fn ResumeTimer(&self) -> windows::core::Result<()> {
        Ok(())
    }
}

/// An item's file-system path, as the shell names it.
fn path_of(item: &IShellItem) -> Option<PathBuf> {
    // SAFETY: `item` is a live interface for the call; the string it returns
    // is the caller's, freed below with CoTaskMemFree after it is copied.
    let name = unsafe { item.GetDisplayName(SIGDN_FILESYSPATH) }.ok()?;
    // SAFETY: GetDisplayName returned a NUL-terminated string.
    let wide = unsafe { name.as_wide() }.to_vec();
    // SAFETY: the string was allocated by the shell with CoTaskMemAlloc.
    unsafe { CoTaskMemFree(Some(name.0.cast_const().cast())) };
    Some(PathBuf::from(std::ffi::OsString::from_wide(&wide)))
}

/// Asks the shell to recycle the item at `path`, a plain path (no `\\?\`),
/// with `flags`, on a thread of its own apartment, and tells what it said.
/// The call cannot be interrupted: it returns when the shell is done.
fn recycle(path: &Path, flags: FILEOPERATION_FLAGS, veto: Veto) -> io::Result<ShellLog> {
    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "a path with a NUL"));
    }
    wide.push(0);
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .name("ddc-recycle".to_owned())
            .spawn_scoped(s, move || operate(&wide, flags, veto))?
            .join()
            .map_err(|_| io::Error::other("the Recycle Bin's thread panicked"))
    })
}

/// The operation itself, on its own thread.
fn operate(path: &[u16], flags: FILEOPERATION_FLAGS, veto: Veto) -> ShellLog {
    // Uninitializes COM last, after every interface below is released.
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            // SAFETY: paired with the CoInitializeEx that succeeded on this thread.
            unsafe { CoUninitialize() };
        }
    }
    let log = Rc::new(RefCell::new(ShellLog::default()));
    // SAFETY: called once on this thread, before any COM call on it.
    let init = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
    if init.is_err() {
        return ShellLog { setup: Some(init), ..ShellLog::default() };
    }
    let _apartment = Apartment;
    let run = || -> windows::core::Result<()> {
        // SAFETY: COM is initialized on this thread; the class and interface
        // are the shell's own.
        let op: IFileOperation = unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_INPROC_SERVER) }?;
        // SAFETY: `op` is live; `path` is NUL-terminated and outlives the call.
        unsafe {
            op.SetOperationFlags(flags)?;
            let item: IShellItem = SHCreateItemFromParsingName(PCWSTR(path.as_ptr()), None)?;
            let sink: IFileOperationProgressSink = Sink { log: Rc::clone(&log), veto }.into();
            op.DeleteItem(&item, &sink)?;
            let performed = op.PerformOperations();
            let aborted = op.GetAnyOperationsAborted().map(|b| b.as_bool()).unwrap_or(false);
            let mut l = log.borrow_mut();
            l.performed = Some(performed.map_or_else(|e| e.code(), |()| HRESULT(0)));
            l.aborted = aborted;
        }
        Ok(())
    };
    if let Err(e) = run() {
        log.borrow_mut().setup = Some(e.code());
    }
    log.borrow().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shell_is_given_a_plain_path_and_never_one_it_would_read_as_another() {
        assert_eq!(shell_path(Path::new(r"\\?\C:\p\node_modules")), Some(PathBuf::from(r"C:\p\node_modules")));
        assert_eq!(shell_path(Path::new(r"C:\p\node_modules")), Some(PathBuf::from(r"C:\p\node_modules")));
        for odd in [r"\\?\C:\p\cache.", r"\\?\C:\p\cache ", r"C:\p\cache.", r"\\?\C:\p\NUL", r"C:\p\com1.txt\x"] {
            assert_eq!(shell_path(Path::new(odd)), None, "{odd}");
        }
        let at = |n: usize| format!(r"C:\{}", "a".repeat(n - 3));
        assert!(shell_path(Path::new(&at(SHELL_PATH_MAX))).is_some(), "259 characters");
        assert!(shell_path(Path::new(&at(SHELL_PATH_MAX + 1))).is_none(), "260 characters");
    }

    fn facts() -> Facts {
        Facts { mount: PathBuf::from(r"T:\"), drive: DRIVE_FIXED, fat: false, max_mb: Some(4), nuke: false }
    }

    const CLEAR: Policy = Policy { no_recycle: false, bin_size: false };

    fn ask(path: &Path, bytes: u64) -> TrashAsk<'_> {
        TrashAsk { name: path.file_name().unwrap(), path, bytes, is_dir: false }
    }

    fn cause(r: Result<u64, TrashError>) -> Option<RecycleBinCause> {
        match r {
            Err(TrashError::RecycleBinRefuses(c)) => Some(c),
            _ => None,
        }
    }

    #[test]
    fn the_static_checks_name_the_first_that_fails() {
        let p = PathBuf::from(r"T:\p\cache");
        let ok = [ask(&p, 1 << 20)];
        assert_eq!(refusal(CLEAR, &facts(), &ok).ok(), Some(4 << 20));
        let all_off = Facts { drive: 2, nuke: true, max_mb: None, ..facts() };
        assert_eq!(
            cause(refusal(Policy { no_recycle: true, bin_size: true }, &all_off, &ok)),
            Some(RecycleBinCause::NoRecycleFiles)
        );
        assert_eq!(cause(refusal(CLEAR, &all_off, &ok)), Some(RecycleBinCause::NoBin), "a removable drive");
        assert!(matches!(
            refusal(CLEAR, &Facts { fat: true, nuke: true, ..facts() }, &ok),
            Err(TrashError::CrossesFilesystem)
        ));
        assert_eq!(
            cause(refusal(CLEAR, &Facts { nuke: true, max_mb: None, ..facts() }, &ok)),
            Some(RecycleBinCause::NukeOnDelete)
        );
        assert_eq!(
            cause(refusal(CLEAR, &Facts { max_mb: None, ..facts() }, &ok)),
            Some(RecycleBinCause::SettingsUnreadable)
        );
        assert_eq!(
            cause(refusal(Policy { bin_size: true, ..CLEAR }, &facts(), &ok)),
            Some(RecycleBinCause::SettingsUnreadable)
        );
        let odd = PathBuf::from(r"T:\p\cache.");
        assert_eq!(
            cause(refusal(CLEAR, &facts(), &[ask(&p, 1), ask(&odd, 1)])),
            Some(RecycleBinCause::PathRefused),
            "any part"
        );
        assert_eq!(cause(refusal(CLEAR, &facts(), &[ask(&p, (4 << 20) + 1)])), Some(RecycleBinCause::TooLarge));
        assert!(
            refusal(CLEAR, &facts(), &[ask(&p, 4 << 20), ask(&p, 4 << 20)]).is_ok(),
            "each part alone fits; the room is the budget's"
        );
    }

    fn post(item: &str, hr: HRESULT, new: Option<&str>) -> Post {
        Post { item: Some(PathBuf::from(item)), hr, new: new.map(PathBuf::from) }
    }

    fn answer(log: &ShellLog, source: Source, bin: &Bin) -> Result<TrashDone, TrashError> {
        classify(log, log.post.first(), source, bin, Path::new(r"C:\p\cache"))
    }

    #[test]
    fn the_shells_answer_and_the_after_check_make_one_result() {
        let new = r"C:\$Recycle.Bin\S-1\$R1";
        let moved =
            ShellLog { post: vec![post(r"C:\p\cache", HRESULT(0x0027_0008), Some(new))], ..ShellLog::default() };
        assert_eq!(
            answer(&moved, Source::Gone, &Bin::Ours(PathBuf::from(new))).unwrap(),
            TrashDone::Moved(Some(TrashPlace { trash: PathBuf::from(r"C:\$Recycle.Bin\S-1"), name: "$R1".into() }))
        );
        let destroyed = ShellLog { post: vec![post(r"C:\p\cache", HRESULT(0), None)], ..ShellLog::default() };
        assert_eq!(answer(&destroyed, Source::Gone, &Bin::None).unwrap(), TrashDone::RemovedNotRecycled);
        assert!(answer(&ShellLog::default(), Source::Gone, &Bin::None).is_err(), "gone with no word from the shell");
        assert!(answer(&moved, Source::Gone, &Bin::Other(PathBuf::from(new))).is_err(), "another item in the bin");
        let refused_with = |hr: HRESULT| {
            let log = ShellLog { post: vec![post(r"C:\p\cache", hr, None)], aborted: true, ..ShellLog::default() };
            answer(&log, Source::Whole, &Bin::None)
        };
        let cause = |r: Result<TrashDone, TrashError>| match r {
            Err(TrashError::RecycleBinRefuses(c)) => Some(c),
            _ => None,
        };
        assert_eq!(cause(refused_with(COPYENGINE_E_RECYCLE_FORCE_NUKE)), Some(RecycleBinCause::WouldDestroy));
        assert_eq!(cause(refused_with(COPYENGINE_E_RECYCLE_SIZE_TOO_BIG)), Some(RecycleBinCause::TooLarge));
        assert_eq!(cause(refused_with(COPYENGINE_E_RECYCLE_PATH_TOO_LONG)), Some(RecycleBinCause::PathRefused));
        assert_eq!(cause(refused_with(COPYENGINE_E_RECYCLE_BIN_NOT_FOUND)), Some(RecycleBinCause::NoBin));
        for hr in
            [COPYENGINE_E_SHARING_VIOLATION_SRC, COPYENGINE_E_SHARING_VIOLATION_DEST, COPYENGINE_E_ACCESS_DENIED_SRC]
        {
            assert!(matches!(refused_with(hr), Err(TrashError::HeldOpen)), "{:#x}", hr.0);
        }
        assert!(matches!(refused_with(HRESULT(0x8027_0035_u32 as i32)), Err(TrashError::Other(_))));
        assert!(matches!(refused_with(HRESULT(0)), Err(TrashError::Other(_))), "done, yet whole");
        let item = Some(PathBuf::from(r"C:\p\cache"));
        let vetoed = ShellLog {
            vetoed: true,
            pre: vec![(item.clone(), 0x2)],
            post: vec![post(r"C:\p\cache", E_ABORT, None)],
            ..ShellLog::default()
        };
        assert_eq!(cause(answer(&vetoed, Source::Whole, &Bin::None)), Some(RecycleBinCause::WouldDestroy));
        let child_vetoed = ShellLog {
            vetoed: true,
            pre: vec![(item, 0x282), (Some(PathBuf::from(r"C:\p\cache\a")), 0x2)],
            post: vec![post(r"C:\p\cache\a", COPYENGINE_S_NOT_HANDLED, None), post(r"C:\p\cache", E_ABORT, None)],
            ..ShellLog::default()
        };
        assert_eq!(
            cause(classify(
                &child_vetoed,
                child_vetoed.post.last(),
                Source::Whole,
                &Bin::None,
                Path::new(r"C:\p\cache")
            )),
            Some(RecycleBinCause::TooLarge),
            "a folder over the maximum, its children about to be destroyed"
        );
        let no_com = ShellLog { setup: Some(HRESULT(0x8000_4005_u32 as i32)), ..ShellLog::default() };
        assert!(
            matches!(answer(&no_com, Source::Whole, &Bin::None), Err(TrashError::Other(e)) if e.to_string().contains("0x80004005"))
        );
        let partly = ShellLog {
            post: vec![
                post(r"C:\p\cache", COPYENGINE_E_SHARING_VIOLATION_DEST, None),
                post(r"C:\p\cache\a", HRESULT(0), None),
            ],
            ..ShellLog::default()
        };
        assert!(
            matches!(answer(&partly, Source::Whole, &Bin::None), Err(TrashError::Other(_))),
            "a folder partly moved"
        );
        assert!(answer(&moved, Source::Changed, &Bin::None).is_err());
    }

    #[test]
    fn a_size_that_cannot_be_read_refuses_and_is_never_met_by_moving() {
        let d = tempfile::tempdir().unwrap();
        let parent = DirFd::open(d.path()).unwrap();
        let missing = d.path().join("missing");
        assert!(matches!(shell_counts(&parent, &ask(&missing, 0), u64::MAX), Err(TrashError::HeldOpen)));
    }

    #[test]
    fn a_registry_value_that_is_absent_reads_as_none() {
        assert_eq!(dword(HKEY_CURRENT_USER, r"Software\ddc-test-absent-key", "Value"), Some(None));
    }

    #[test]
    fn the_flags_ask_for_no_ui_and_the_recycle_bin() {
        assert_eq!(FLAGS.0 & FOF_NO_UI.0, FOF_NO_UI.0);
        assert_ne!(FLAGS.0 & FOFX_RECYCLEONDELETE.0, 0);
        assert_ne!(FLAGS.0 & FOF_ALLOWUNDO.0, 0);
        assert_ne!(FLAGS.0 & FOFX_NOCOPYHOOKS.0, 0);
    }
}

/// The lab experiments of step 10b, run by hand on the lab machine:
/// `scripts/native-windows.sh --run-ignored only -E 'test(/trash::lab::/)'
/// --no-capture`. Each prints one line per case: the shell's results, the
/// source's state and where the item went, the account's SID replaced by
/// `<sid>`. ddc's own items are removed from the bin by the names the shell
/// gave them; the bin is never listed and its totals are never printed.
#[cfg(test)]
#[allow(clippy::print_stdout, clippy::disallowed_methods)] // the lab's own report; its own test files
mod lab {
    use std::fs;
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::process::{Child, Command, Stdio};

    use super::*;

    /// A folder of the lab's own under the temporary folder, removed after.
    struct Lab(PathBuf);

    impl Lab {
        fn new(case: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("ddc-lab-{case}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, rel: &str, bytes: usize) -> PathBuf {
            let path = self.0.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, vec![b'x'; bytes]).unwrap();
            path
        }
    }

    impl Drop for Lab {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The bin item's path with the account's SID folder hidden.
    fn shown(path: &Path) -> String {
        let text = path.display().to_string();
        match text.find("$Recycle.Bin\\") {
            Some(i) => {
                let rest = &text[i + "$Recycle.Bin\\".len()..];
                let after = rest.find('\\').map_or("", |j| &rest[j..]);
                format!("{}$Recycle.Bin\\<sid>{after}", &text[..i])
            }
            None => text,
        }
    }

    /// Removes the bin items the shell reported, `$R…` and its `$I…`, by name.
    fn take_back(log: &ShellLog) {
        for new in log.post.iter().filter_map(|p| p.new.as_ref()) {
            let Some(name) = new.file_name().and_then(|n| n.to_str()) else { continue };
            if let Some(tail) = name.strip_prefix("$R") {
                let info = new.with_file_name(format!("$I{tail}"));
                let _ = if new.is_dir() { fs::remove_dir_all(new) } else { fs::remove_file(new) };
                let _ = fs::remove_file(info);
            }
        }
    }

    fn report(case: &str, log: &ShellLog, source: &Path) {
        let state = if source.exists() { "present" } else { "gone" };
        let pre: Vec<String> = log.pre.iter().map(|(_, f)| format!("{f:#x}")).collect();
        let post: Vec<String> = log
            .post
            .iter()
            .map(|p| format!("hr={:#010x} new={}", p.hr.0, p.new.as_deref().map_or("none".to_owned(), shown)))
            .collect();
        println!(
            "LAB {case}: setup={:?} performed={:?} aborted={} vetoed={} pre=[{}] post=[{}] source={state}",
            log.setup.map(|h| format!("{:#010x}", h.0)),
            log.performed.map(|h| format!("{:#010x}", h.0)),
            log.aborted,
            log.vetoed,
            pre.join(", "),
            post.join("; ")
        );
    }

    fn id(path: &Path) -> Option<(u64, u64)> {
        super::super::meta::lstat(path).ok().map(|st| st.id())
    }

    /// S: the shell's move works over OpenSSH, reports the item in the bin,
    /// and that item keeps the source's file id.
    #[test]
    #[ignore = "a lab experiment: moves into the user's Recycle Bin"]
    fn s_a_file_goes_to_the_bin_and_keeps_its_id() {
        let lab = Lab::new("s");
        let file = lab.file("a.txt", 4096);
        let before = id(&file);
        let log = recycle(&file, FLAGS, Veto::WouldDestroy).unwrap();
        report("S file", &log, &file);
        let new = log.post.iter().find_map(|p| p.new.clone());
        println!("LAB S id kept: {}", new.as_deref().and_then(id) == before && before.is_some());
        take_back(&log);

        let dir = lab.0.join("d");
        lab.file("d/x/y.txt", 1000);
        let before = id(&dir);
        let log = recycle(&dir, FLAGS, Veto::WouldDestroy).unwrap();
        report("S folder", &log, &dir);
        let new = log.post.iter().find_map(|p| p.new.clone());
        println!("LAB S folder id kept: {}", new.as_deref().and_then(id) == before && before.is_some());
        take_back(&log);
    }

    /// The whole path on C:: `admit` then `trash_item`, a file moved and a
    /// folder held open refused, through the verified folder.
    #[test]
    #[ignore = "a lab experiment: moves into the user's Recycle Bin"]
    fn p_admit_and_trash_item_on_the_system_drive() {
        let lab = Lab::new("p");
        let file = lab.file("a.txt", 4096);
        let parent = DirFd::open(&lab.0).unwrap();
        let mut budget = Budget::default();
        let name = file.file_name().unwrap();
        let ask = TrashAsk { name, path: &file, bytes: 4096, is_dir: false };
        println!("LAB P admit: {:?}", admit(&mut budget, &parent, &[ask]).map_err(|e| e.to_string()));
        let done = trash_item(&parent, name, &file, id(&file).unwrap());
        println!(
            "LAB P file: {:?} source={}",
            done.as_ref()
                .map(|d| match d {
                    TrashDone::Moved(Some(p)) => format!("moved to {}", shown(&p.trash.join(&p.name))),
                    other => format!("{other:?}"),
                })
                .map_err(ToString::to_string),
            if file.exists() { "present" } else { "gone" }
        );
        if let Ok(TrashDone::Moved(Some(place))) = &done {
            let log = ShellLog {
                post: vec![Post { item: None, hr: HRESULT(0), new: Some(place.trash.join(&place.name)) }],
                ..ShellLog::default()
            };
            take_back(&log);
        }
        let dir = lab.0.join("held");
        let held = lab.file("held/f.txt", 100);
        let guard = holding(&held, 1);
        let done = trash_item(&parent, OsStr::new("held"), &dir, id(&dir).unwrap());
        println!(
            "LAB P held: {:?} source={}",
            done.map_err(|e| e.to_string()),
            if dir.exists() { "present" } else { "gone" }
        );
        drop(guard);
    }

    /// L0-veto: a sink that refuses every item leaves the source whole.
    #[test]
    #[ignore = "a lab experiment: the Recycle Bin's move"]
    fn l0_a_veto_leaves_the_source_whole() {
        let lab = Lab::new("veto");
        let file = lab.file("a.txt", 4096);
        let log = recycle(&file, FLAGS, Veto::Always).unwrap();
        report("L0 veto", &log, &file);
        take_back(&log);
    }

    /// Holds `path` open with `share`, as another program would.
    fn holding(path: &Path, share: u32) -> fs::File {
        fs::OpenOptions::new().read(true).share_mode(share).open(path).unwrap()
    }

    fn sleeper(cwd: &Path, exe: Option<&Path>) -> Child {
        let (program, args) = super::super::testing::sleep_command(30);
        let program = exe.map_or(program, Path::to_path_buf);
        Command::new(program).args(args).current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::null()).spawn().unwrap()
    }

    /// L1: a folder with an open file below, a process working inside, a
    /// running program inside, a long path inside, and a folder of which
    /// only one file is held.
    #[test]
    #[ignore = "a lab experiment: moves into the user's Recycle Bin"]
    fn l1_held_folders_and_long_paths() {
        const READ: u32 = 1; // FILE_SHARE_READ
        const READ_WRITE_DELETE: u32 = 7;
        let lab = Lab::new("l1");

        let open = lab.0.join("open");
        let held = lab.file("open/f.txt", 1000);
        let guard = holding(&held, READ);
        let log = recycle(&open, FLAGS, Veto::WouldDestroy).unwrap();
        report("L1 open file below (share read)", &log, &open);
        drop(guard);
        take_back(&log);

        let open = lab.0.join("open-del");
        let held = lab.file("open-del/f.txt", 1000);
        let guard = holding(&held, READ_WRITE_DELETE);
        let log = recycle(&open, FLAGS, Veto::WouldDestroy).unwrap();
        report("L1 open file below (share delete)", &log, &open);
        drop(guard);
        take_back(&log);

        let work = lab.0.join("work");
        lab.file("work/f.txt", 1000);
        let mut child = sleeper(&work, None);
        std::thread::sleep(std::time::Duration::from_millis(500));
        let log = recycle(&work, FLAGS, Veto::WouldDestroy).unwrap();
        report("L1 working folder", &log, &work);
        let _ = child.kill();
        let _ = child.wait();
        take_back(&log);

        let run = lab.0.join("run");
        fs::create_dir_all(&run).unwrap();
        let (program, _) = super::super::testing::sleep_command(30);
        let exe = run.join(program.file_name().unwrap());
        fs::copy(&program, &exe).unwrap();
        let mut child = sleeper(&lab.0, Some(&exe));
        std::thread::sleep(std::time::Duration::from_millis(500));
        let log = recycle(&run, FLAGS, Veto::WouldDestroy).unwrap();
        report("L1 running program inside", &log, &run);
        let _ = child.kill();
        let _ = child.wait();
        take_back(&log);

        let long = lab.0.join("long");
        let deep: PathBuf = (0..12).map(|i| format!("segment-{i:02}-xxxxxxxxxxxxxxxx")).collect();
        let inner = long.join(&deep).join("f.txt");
        let verbatim = PathBuf::from(format!(r"\\?\{}", inner.display()));
        fs::create_dir_all(verbatim.parent().unwrap()).unwrap();
        fs::write(&verbatim, b"x").unwrap();
        println!("LAB L1 long: inner path {} characters", inner.as_os_str().len());
        let log = recycle(&long, FLAGS, Veto::WouldDestroy).unwrap();
        report("L1 long path inside", &log, &long);
        take_back(&log);

        let part = lab.0.join("part");
        lab.file("part/a.txt", 1000);
        let held = lab.file("part/z.txt", 1000);
        let guard = holding(&held, READ);
        let log = recycle(&part, FLAGS, Veto::WouldDestroy).unwrap();
        report("L1 one file of two held", &log, &part);
        drop(guard);
        take_back(&log);
    }
}
