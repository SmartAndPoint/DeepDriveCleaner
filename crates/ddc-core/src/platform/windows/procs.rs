//! Processes and this process's token, on Windows.
//!
//! The process table (story "Windows scanner and rule catalog", step 9a;
//! safety ADR §5, amendment of 2026-11-02, the process check): the user's
//! processes from a Toolhelp snapshot. A process is the user's when its
//! token's user SID equals this process's; one whose token cannot be read,
//! or that cannot be opened, is not counted. For each: the executable
//! (`QueryFullProcessImageNameW`, the Win32 form), the command line, the
//! working folder and the environment ([`super::peb`]), each path without
//! the `\\?\` prefix. What cannot be read is left empty with `withheld`
//! set. The loaded modules are its open files ([`super::modules`], step
//! 9b), but for this process and its children; the open handles
//! (`open_vnodes`) are not read: a per-process handle list needs
//! `PROCESS_DUP_HANDLE` on every process, and naming a pipe's handle can
//! hang.
//!
//! The token is ported (story "Windows scanner and rule catalog", step 7):
//!
//! - [`is_root`] is true only for LocalSystem, the account of a service.
//!   An elevated token ("Run as administrator") is not root: it may scan
//!   and clean the user's own folders.
//! - [`root_owner`] is the owner check of an elevated session: a scan root
//!   another account owns (the Windows folder, another user's profile) is
//!   refused by `platform::require_own_root`, since elevation would let ddc
//!   read and change it. A token that is not elevated has no more rights
//!   than the user, and the path is not even opened.
//!
//! - [`drop_acl_bypass_privileges`] removes from the token the three
//!   privileges that pass every ACL: SeBackupPrivilege, SeRestorePrivilege
//!   and SeTakeOwnershipPrivilege. The CLI calls it at its start, and the
//!   token is read only after it, so no command runs with them. An elevated
//!   OpenSSH session starts with all three enabled; an elevated console
//!   holds them disabled. A token that still holds one after the removal
//!   is refused by [`root_owner`], so nothing is scanned or cleaned.
//!
//! SIDs are compared and printed as bytes; no account name is looked up
//! (a lookup can ask a domain controller and wait on the network). Nothing
//! enables a privilege (a structure test refuses the calls that would,
//! outside this file's removal).

use std::io;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use std::os::windows::ffi::OsStringExt as _;

use windows_sys::Win32::Foundation::{
    ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, FILETIME, HANDLE, INVALID_HANDLE_VALUE, LUID, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, GetLengthSid, GetTokenInformation, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_BACKUP_NAME, SE_PRIVILEGE_REMOVED, SE_RESTORE_NAME,
    SE_TAKE_OWNERSHIP_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_ELEVATION, TOKEN_INFORMATION_CLASS, TOKEN_OWNER,
    TOKEN_PRIVILEGES, TOKEN_QUERY, TOKEN_USER, TokenElevation, TokenOwner, TokenPrivileges, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, READ_CONTROL};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetProcessTimes, OpenProcess, OpenProcessToken, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ, QueryFullProcessImageNameW,
};

pub use crate::platform::types::{OpenVnode, ProcInfo, ProcStart, RootOwner, TokenUnusable, app_bundle};

/// Windows has no user id: this stands for the user, as [`process_start`]
/// gives it for the user's processes. Owners are never compared by it
/// (`meta::owner` is `None` here).
pub fn uid() -> u32 {
    u32::MAX
}

/// [`ProcStart::uid`] of a process of another account: never [`uid`].
const OTHER_UID: u32 = u32::MAX - 1;

pub fn real_home() -> Option<PathBuf> {
    None
}

/// Whether the process runs as LocalSystem (the executor refuses to). Never
/// "elevated": an administrator's terminal is not root. False when the
/// token is unusable; [`root_owner`] then fails, and so does every scan.
pub fn is_root() -> bool {
    token().is_ok_and(|t| is_local_system(&t.user))
}

/// Whose `root` is, for a scan or a clean of it. [`RootOwner::Own`] without
/// opening the path when the token is not elevated; otherwise the owner of
/// the root's security descriptor, read through its own handle (the link
/// itself, never what it names; never a download of a cloud placeholder).
/// The owner is the user's when it is the token's user or the token's
/// default owner (the Administrators group, for an administrator's elevated
/// token: what it creates belongs to that group). A volume that keeps no
/// owner (FAT, exFAT) is the user's. An unusable token (unreadable, or one
/// that still holds a privilege that passes every ACL) is an error whose
/// payload is a [`TokenUnusable`].
pub fn root_owner(root: &Path) -> io::Result<RootOwner> {
    let token = token()?;
    if !token.elevated {
        return Ok(RootOwner::Own);
    }
    let owner = owner_of(root)?;
    Ok(verdict(true, owner.as_deref(), &token.user, &token.owner))
}

/// The decision of [`root_owner`], with every SID as its bytes.
fn verdict(elevated: bool, owner: Option<&[u8]>, user: &[u8], token_owner: &[u8]) -> RootOwner {
    match owner {
        _ if !elevated => RootOwner::Own,
        None => RootOwner::Own,
        Some(owner) if owner == user || owner == token_owner => RootOwner::Own,
        Some(owner) => RootOwner::Other(sid_string(owner)),
    }
}

/// S-1-5-18, LocalSystem.
const LOCAL_SYSTEM: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 5, 18, 0, 0, 0];

fn is_local_system(sid: &[u8]) -> bool {
    sid == LOCAL_SYSTEM
}

/// A SID in its string form, `S-1-5-21-…`: revision, the 48-bit authority
/// (big-endian; in hex above 32 bits, as Windows prints it) and each
/// sub-authority (little-endian). Bytes that are no SID are shown in hex.
fn sid_string(sid: &[u8]) -> String {
    let hex = || sid.iter().map(|b| format!("{b:02x}")).collect::<String>();
    let (Some(&revision), Some(&count)) = (sid.first(), sid.get(1)) else { return hex() };
    if sid.len() != 8 + 4 * usize::from(count) {
        return hex();
    }
    let authority = sid[2..8].iter().fold(0u64, |acc, b| (acc << 8) | u64::from(*b));
    let mut text = if authority >> 32 == 0 {
        format!("S-{revision}-{authority}")
    } else {
        format!("S-{revision}-0x{authority:012X}")
    };
    for sub in sid[8..].as_chunks::<4>().0 {
        text.push_str(&format!("-{}", u32::from_le_bytes(*sub)));
    }
    text
}

/// What this process's token says, read once.
struct Token {
    elevated: bool,
    user: Vec<u8>,
    /// The default owner of what the token creates.
    owner: Vec<u8>,
}

/// The token, read once, after [`drop_acl_bypass_privileges`].
fn token() -> io::Result<&'static Token> {
    static TOKEN: OnceLock<Result<Token, String>> = OnceLock::new();
    TOKEN
        .get_or_init(|| {
            let dropped = drop_acl_bypass_privileges();
            read_token(dropped.err())
        })
        .as_ref()
        .map_err(|e| io::Error::other(TokenUnusable(e.clone())))
}

/// The privileges that pass every ACL, by name: backup (read anything),
/// restore (write anything) and take ownership.
const ACL_BYPASS: [(windows_sys::core::PCWSTR, &str); 3] = [
    (SE_BACKUP_NAME, "SeBackupPrivilege"),
    (SE_RESTORE_NAME, "SeRestorePrivilege"),
    (SE_TAKE_OWNERSHIP_NAME, "SeTakeOwnershipPrivilege"),
];

/// Removes the [`ACL_BYPASS`] privileges from this process's token for the
/// rest of its life (`SE_PRIVILEGE_REMOVED`: they cannot be enabled again),
/// then reads the token's privileges again: an error when it still lists
/// one. One the token never held is no error.
pub fn drop_acl_bypass_privileges() -> io::Result<()> {
    let mut raw: HANDLE = std::ptr::null_mut();
    // SAFETY: the current process's pseudo-handle; `raw` is a writable local.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY | TOKEN_ADJUST_PRIVILEGES, &mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a handle OpenProcessToken just returned, owned by nobody else.
    let token = unsafe { OwnedHandle::from_raw_handle(raw) };
    for (name, _) in ACL_BYPASS {
        let removed = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES { Luid: privilege(name)?, Attributes: SE_PRIVILEGE_REMOVED }],
        };
        // SAFETY: the token is open with TOKEN_ADJUST_PRIVILEGES; `removed`
        // is one whole entry; no previous state is asked for, so its buffer
        // and length are null and 0. A privilege the token does not hold
        // succeeds with ERROR_NOT_ALL_ASSIGNED, which is fine here.
        let ok = unsafe {
            AdjustTokenPrivileges(token.as_raw_handle(), 0, &removed, 0, std::ptr::null_mut(), std::ptr::null_mut())
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    // The call's success says little (one not held succeeds too): the
    // token itself must no longer list any of them.
    let held: Vec<&str> =
        acl_bypass(&token)?.iter().zip(ACL_BYPASS).filter(|(a, _)| a.is_some()).map(|(_, (_, n))| n).collect();
    if held.is_empty() { Ok(()) } else { Err(io::Error::other(format!("the token still holds {}", held.join(", ")))) }
}

/// The local id of the privilege named `name`.
fn privilege(name: windows_sys::core::PCWSTR) -> io::Result<LUID> {
    let mut luid = LUID { LowPart: 0, HighPart: 0 };
    // SAFETY: a null system name is the local one; `name` is a static
    // NUL-terminated wide string; `luid` is a writable local.
    if unsafe { LookupPrivilegeValueW(std::ptr::null(), name, &mut luid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(luid)
}

/// The attributes of each [`ACL_BYPASS`] privilege the token holds, in
/// that order; `None` for one it does not hold.
fn acl_bypass(token: &OwnedHandle) -> io::Result<[Option<u32>; 3]> {
    let info = token_info(token, TokenPrivileges)?;
    let fixed: TOKEN_PRIVILEGES = head(&info)?;
    let count = fixed.PrivilegeCount as usize;
    let offset = std::mem::offset_of!(TOKEN_PRIVILEGES, Privileges);
    if offset + count * size_of::<LUID_AND_ATTRIBUTES>() > info.len {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "a short list of privileges"));
    }
    // SAFETY: GetTokenInformation wrote `count` entries after the count, all
    // inside the buffer (checked above), aligned as the struct is.
    let held = unsafe {
        std::slice::from_raw_parts(info.buf.as_ptr().cast::<u8>().add(offset).cast::<LUID_AND_ATTRIBUTES>(), count)
    };
    let mut out = [None; 3];
    for (slot, (name, _)) in out.iter_mut().zip(ACL_BYPASS) {
        let luid = privilege(name)?;
        *slot = held
            .iter()
            .find(|p| p.Luid.LowPart == luid.LowPart && p.Luid.HighPart == luid.HighPart)
            .map(|p| p.Attributes);
    }
    Ok(out)
}

/// The token, or why it is unusable: unreadable, or still holding an
/// [`ACL_BYPASS`] privilege after `dropped` (the removal's error, if any).
fn read_token(dropped: Option<io::Error>) -> Result<Token, String> {
    let read = || -> io::Result<(Token, [Option<u32>; 3])> {
        let token = open_token()?;
        Ok((read_from(&token)?, acl_bypass(&token)?))
    };
    let (token, bypass) = read().map_err(|e| format!("cannot read this process's token: {e}"))?;
    let held: Vec<&str> = bypass.iter().zip(ACL_BYPASS).filter(|(a, _)| a.is_some()).map(|(_, (_, n))| n).collect();
    if held.is_empty() {
        return Ok(token);
    }
    let why = dropped.map_or_else(|| "it was not removed".to_owned(), |e| format!("its removal failed: {e}"));
    Err(format!("this process's token still holds {}, which passes every ACL ({why})", held.join(", ")))
}

fn read_from(token: &OwnedHandle) -> io::Result<Token> {
    let elevated = elevated(token)?;
    let user_info = token_info(token, TokenUser)?;
    let user: TOKEN_USER = head(&user_info)?;
    let owner_info = token_info(token, TokenOwner)?;
    let owner: TOKEN_OWNER = head(&owner_info)?;
    // SAFETY: both SIDs point into their buffer, alive here, which
    // GetTokenInformation filled with the class's struct and its SID.
    let (user, owner) = unsafe { (sid_bytes(user.User.Sid)?, sid_bytes(owner.Owner)?) };
    Ok(Token { elevated, user, owner })
}

/// Whether the token is elevated: a class of a fixed size, which has no
/// size query (ERROR_BAD_LENGTH), so it is read into its struct.
fn elevated(token: &OwnedHandle) -> io::Result<bool> {
    let mut elevation = TOKEN_ELEVATION::default();
    let size = u32::try_from(size_of::<TOKEN_ELEVATION>()).expect("a small struct");
    let mut len = 0u32;
    // SAFETY: `elevation` is writable for `size` bytes, the struct of the
    // class; `len` is a writable local.
    let ok = unsafe {
        GetTokenInformation(token.as_raw_handle(), TokenElevation, (&raw mut elevation).cast(), size, &mut len)
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(elevation.TokenIsElevated != 0)
}

/// This process's token, open for queries only.
fn open_token() -> io::Result<OwnedHandle> {
    let mut raw: HANDLE = std::ptr::null_mut();
    // SAFETY: the current process's pseudo-handle; `raw` is a writable local.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a handle OpenProcessToken just returned, owned by nobody else.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

/// One class of the token's information of a variable size, in a buffer
/// aligned for `usize` (every token struct is, and its SIDs point into it),
/// with the length written.
struct Info {
    buf: Vec<usize>,
    len: usize,
}

fn token_info(token: &OwnedHandle, class: TOKEN_INFORMATION_CLASS) -> io::Result<Info> {
    let mut len = 0u32;
    // SAFETY: a null buffer of length 0 asks for the size only, written to
    // `len`; the call fails with ERROR_INSUFFICIENT_BUFFER, as expected.
    unsafe { GetTokenInformation(token.as_raw_handle(), class, std::ptr::null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buf = vec![0usize; (len as usize).div_ceil(size_of::<usize>())];
    let size =
        u32::try_from(buf.len() * size_of::<usize>()).map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
    // SAFETY: `buf` is writable for `size` bytes and outlives the call.
    if unsafe { GetTokenInformation(token.as_raw_handle(), class, buf.as_mut_ptr().cast(), size, &mut len) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Info { buf, len: len as usize })
}

/// The struct at the start of `info`: `T` must be its class's struct.
fn head<T: Copy>(info: &Info) -> io::Result<T> {
    const { assert!(align_of::<T>() <= align_of::<usize>()) };
    if info.len < size_of::<T>() || info.len > info.buf.len() * size_of::<usize>() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "a short token information"));
    }
    // SAFETY: the buffer is aligned for `T` (asserted above) and holds at
    // least `size_of::<T>()` bytes GetTokenInformation wrote as a `T`.
    Ok(unsafe { info.buf.as_ptr().cast::<T>().read() })
}

/// The bytes of the SID at `sid`, copied.
///
/// # Safety
///
/// `sid` is null or points to a SID that stays valid for the call.
unsafe fn sid_bytes(sid: PSID) -> io::Result<Vec<u8>> {
    if sid.is_null() {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "a null SID"));
    }
    // SAFETY: a valid SID, by the caller.
    let len = unsafe { GetLengthSid(sid) } as usize;
    // SAFETY: GetLengthSid gives the SID's own length, all readable.
    Ok(unsafe { std::slice::from_raw_parts(sid.cast::<u8>(), len) }.to_vec())
}

/// The owner SID of what `path` names (the link itself), or `None` when its
/// file system keeps none.
fn owner_of(path: &Path) -> io::Result<Option<Vec<u8>>> {
    let handle = super::meta::open_with(path, READ_CONTROL | FILE_READ_ATTRIBUTES, FILE_FLAG_OPEN_REPARSE_POINT)?;
    let mut owner: PSID = std::ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: the handle is open with READ_CONTROL; `owner` and `descriptor`
    // are writable locals; the group and ACL pointers are null, which asks
    // for none of them.
    let rc = unsafe {
        GetSecurityInfo(
            handle.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut descriptor,
        )
    };
    if rc != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    // SAFETY: `owner` is null or points into `descriptor`, freed only below.
    let sid = if owner.is_null() { Ok(None) } else { unsafe { sid_bytes(owner) }.map(Some) };
    // SAFETY: the descriptor GetSecurityInfo allocated for this call, freed
    // once; nothing points into it after the copy above.
    unsafe { LocalFree(descriptor) };
    sid
}

/// All processes of the user, as the module doc says.
pub fn current_user_processes() -> Vec<ProcInfo> {
    user_processes(true)
}

/// [`current_user_processes`] without their open files: the cost of a list
/// that only names them.
pub fn current_user_processes_unopened() -> Vec<ProcInfo> {
    user_processes(false)
}

fn user_processes(with_files: bool) -> Vec<ProcInfo> {
    let Some(user) = user_sid() else { return Vec::new() };
    let own = std::process::id();
    let own_start = open_limited(own).ok().as_ref().and_then(creation_time);
    let devices = with_files.then(super::modules::Devices::read);
    snapshot()
        .into_iter()
        .filter_map(|entry| {
            let (handle, full) = open(entry.pid)?;
            if owner(&handle).ok()? != user {
                return None;
            }
            let mut info = describe(&entry, &handle, full);
            // This process and the tools it runs hold the clean's own files.
            // Windows never re-parents, so a child is one started after this
            // process whose parent pid is this one's.
            let child = entry.ppid == own && own_start.is_some_and(|own| creation_time(&handle) >= Some(own));
            if let Some(devices) = devices.as_ref().filter(|_| entry.pid != own && !child) {
                match full.then(|| super::modules::of(entry.pid, &handle, devices)).flatten() {
                    Some(modules) => {
                        info.withheld |= !modules.complete;
                        info.open_files = modules.files;
                    }
                    None => info.withheld = true,
                }
            }
            Some(info)
        })
        .collect()
}

/// The modules `pid` (a process of the user) has loaded, as its open files
/// (see [`super::modules`]); empty when it is gone, another account's, or
/// withholds them.
pub fn open_files(pid: i32) -> Vec<PathBuf> {
    let Some(pid) = u32::try_from(pid).ok() else { return Vec::new() };
    let Some((handle, true)) = open(pid) else { return Vec::new() };
    if user_sid().is_none_or(|user| owner(&handle).ok().as_deref() != Some(user)) {
        return Vec::new();
    }
    // What could be read: a module that went meanwhile never hides another.
    super::modules::of(pid, &handle, &super::modules::Devices::read()).map(|m| m.files).unwrap_or_default()
}

/// Not read on Windows: see the module doc.
pub fn open_vnodes(_pid: i32) -> Vec<OpenVnode> {
    Vec::new()
}

/// The start of `pid`, of any user; `None` when there is no such process.
/// A process whose token cannot be read counts as the user's: the start
/// time already tells a reused pid, so a live session is never lost to it.
pub fn process_start(pid: i32) -> Option<ProcStart> {
    let handle = open_limited(u32::try_from(pid).ok()?).ok()?;
    let created = creation_time(&handle)?;
    let uid = match (owner(&handle), user_sid()) {
        (Ok(owner), Some(user)) if owner != user => OTHER_UID,
        _ => uid(),
    };
    Some(ProcStart { uid, sec: created / 10_000_000, usec: created % 10_000_000 / 10 })
}

/// Threads of `pid`, from the snapshot.
pub fn thread_count(pid: i32) -> Option<u32> {
    let pid = u32::try_from(pid).ok()?;
    snapshot().into_iter().find(|e| e.pid == pid).map(|e| e.threads)
}

/// One process of the Toolhelp snapshot.
struct Entry {
    pid: u32,
    ppid: u32,
    threads: u32,
    /// The executable's file name, as the snapshot gives it.
    exe_file: String,
}

/// Every process but the idle process and System (pids 0 and 4), which no
/// user owns.
fn snapshot() -> Vec<Entry> {
    // SAFETY: plain arguments; the handle is checked below.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    // SAFETY: a handle CreateToolhelp32Snapshot just returned, owned by nobody else.
    let snap = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut entry =
        PROCESSENTRY32W { dwSize: u32::try_from(size_of::<PROCESSENTRY32W>()).expect("small"), ..Default::default() };
    let mut out = Vec::new();
    // SAFETY: the snapshot is open; `entry` is a local with its size set.
    let mut more = unsafe { Process32FirstW(snap.as_raw_handle(), &mut entry) } != 0;
    while more {
        if entry.th32ProcessID > 4 {
            let len = entry.szExeFile.iter().position(|&u| u == 0).unwrap_or(entry.szExeFile.len());
            out.push(Entry {
                pid: entry.th32ProcessID,
                ppid: entry.th32ParentProcessID,
                threads: entry.cntThreads,
                exe_file: String::from_utf16_lossy(&entry.szExeFile[..len]),
            });
        }
        // SAFETY: as for Process32FirstW.
        more = unsafe { Process32NextW(snap.as_raw_handle(), &mut entry) } != 0;
    }
    out
}

/// `pid` open to read its parameters, or with a limited handle when that is
/// refused (a protected process, or one of a higher integrity level): the
/// handle and whether it reads memory. `None` when it cannot be opened at
/// all (gone, or another account's).
fn open(pid: u32) -> Option<(OwnedHandle, bool)> {
    match open_with(pid, PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ) {
        Ok(handle) => Some((handle, true)),
        Err(_) => open_limited(pid).ok().map(|h| (h, false)),
    }
}

fn open_limited(pid: u32) -> io::Result<OwnedHandle> {
    open_with(pid, PROCESS_QUERY_LIMITED_INFORMATION)
}

fn open_with(pid: u32, access: u32) -> io::Result<OwnedHandle> {
    // SAFETY: plain arguments; the handle is checked below.
    let raw = unsafe { OpenProcess(access, 0, pid) };
    if raw.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a handle OpenProcess just returned, owned by nobody else.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

/// The user SID of this process's token, read once and apart from the
/// privilege check ([`token`]), so a refused privilege never empties the
/// table.
fn user_sid() -> Option<&'static [u8]> {
    static USER: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    USER.get_or_init(|| user_of(&open_token().ok()?).ok()).as_deref()
}

/// The user SID of the process `handle` is open on.
fn owner(handle: &OwnedHandle) -> io::Result<Vec<u8>> {
    let mut raw: HANDLE = std::ptr::null_mut();
    // SAFETY: the process handle is open for the call; `raw` is a local.
    if unsafe { OpenProcessToken(handle.as_raw_handle(), TOKEN_QUERY, &mut raw) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a handle OpenProcessToken just returned, owned by nobody else.
    let token = unsafe { OwnedHandle::from_raw_handle(raw) };
    user_of(&token)
}

fn user_of(token: &OwnedHandle) -> io::Result<Vec<u8>> {
    let info = token_info(token, TokenUser)?;
    let user: TOKEN_USER = head(&info)?;
    // SAFETY: the SID points into `info`, alive here.
    unsafe { sid_bytes(user.User.Sid) }
}

/// The process's creation time in 100 ns units since the Unix epoch.
fn creation_time(handle: &OwnedHandle) -> Option<u64> {
    let zero = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let (mut created, mut exited, mut kernel, mut user) = (zero, zero, zero, zero);
    // SAFETY: the handle is open for the call; the outputs are locals.
    if unsafe { GetProcessTimes(handle.as_raw_handle(), &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return None;
    }
    const UNITS_1601_TO_1970: u64 = 11_644_473_600 * 10_000_000;
    let since_1601 = u64::from(created.dwHighDateTime) << 32 | u64::from(created.dwLowDateTime);
    since_1601.checked_sub(UNITS_1601_TO_1970)
}

/// What the check reads of one process of the user.
fn describe(entry: &Entry, handle: &OwnedHandle, full: bool) -> ProcInfo {
    let mut info = ProcInfo { pid: i32::try_from(entry.pid).unwrap_or(i32::MAX), ..ProcInfo::default() };
    let exe = image(handle);
    info.name = exe
        .as_deref()
        .and_then(Path::file_name)
        .map_or_else(|| entry.exe_file.clone(), |n| n.to_string_lossy().into_owned());
    info.resolved_exe = exe.clone();
    info.exe = exe;
    match super::peb::command_line(handle) {
        Ok(line) => info.args = super::peb::split_args(&line),
        Err(_) => info.withheld = true,
    }
    let fields = if full { super::peb::fields(handle) } else { super::peb::Fields::default() };
    info.cwd = fields.cwd.map(|cwd| local_canonical(super::paths::simplified(cwd)));
    info.env_prefixes = fields
        .env
        .as_deref()
        .map(super::peb::env_prefixes)
        .unwrap_or_default()
        .into_iter()
        .map(super::paths::simplified)
        .collect();
    // A WOW64 process's fields are never read: withheld, as unreadable ones are.
    info.withheld |= fields.wow64 || fields.env_cut || info.exe.is_none() || info.cwd.is_none() || fields.env.is_none();
    info
}

/// The executable's path, in its Win32 form.
fn image(handle: &OwnedHandle) -> Option<PathBuf> {
    let mut buf = vec![0u16; 1024];
    loop {
        let mut len = u32::try_from(buf.len()).expect("bounded");
        // SAFETY: `buf` is writable for `len` units; `len` is a local.
        let ok = unsafe {
            QueryFullProcessImageNameW(handle.as_raw_handle(), PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
        };
        if ok != 0 {
            let path = PathBuf::from(std::ffi::OsString::from_wide(&buf[..len as usize]));
            return Some(super::paths::simplified(path));
        }
        if io::Error::last_os_error().raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) || buf.len() >= 32_768 {
            return None;
        }
        buf.resize(buf.len() * 4, 0);
    }
}

/// `path` canonicalized without recall when it is on a local volume, so an
/// 8.3 name or a `subst` drive meets its finding; as it is otherwise.
fn local_canonical(path: PathBuf) -> PathBuf {
    if super::paths::is_local(&path) { super::paths::canonicalize(&path).unwrap_or(path) } else { path }
}

/// Not read on Windows, whose builds have no slot (safety ADR §5,
/// amendment of 2026-11-02): no names, and [`process_table_hidden`] says
/// the table is unknown, so a caller that needs it fails closed.
pub fn other_user_names() -> Vec<String> {
    Vec::new()
}

/// Unknown here, so hidden: a caller that needs the table fails closed.
pub fn process_table_hidden() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same(a: &Path, b: &Path) -> bool {
        super::super::paths::key(a) == super::super::paths::key(b)
    }

    fn canonical(p: &Path) -> PathBuf {
        super::super::paths::canonicalize(p).unwrap()
    }

    /// This process is in its own table, with its executable, working
    /// folder and arguments, and its start and threads read.
    #[test]
    fn this_process_is_listed_as_the_users_with_what_it_runs() {
        let own = i32::try_from(std::process::id()).unwrap();
        let table = current_user_processes();
        let me = table.iter().find(|p| p.pid == own).expect("this process is the user's");
        assert!(same(me.exe.as_deref().unwrap(), &canonical(&std::env::current_exe().unwrap())), "{me:?}");
        assert!(same(me.cwd.as_deref().unwrap(), &canonical(&std::env::current_dir().unwrap())), "{me:?}");
        assert!(!me.args.is_empty() && !me.withheld, "{me:?}");
        assert_eq!(Some(me.name.as_str()), me.exe.as_deref().and_then(Path::file_name).and_then(|n| n.to_str()));
        let start = process_start(own).unwrap();
        assert_eq!(start.uid, uid());
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        assert!(start.sec <= now && now - start.sec < 86_400, "started within a day: {start:?}");
        assert!(thread_count(own).is_some_and(|n| n >= 1));
        assert!(process_start(i32::MAX).is_none(), "no such process");
    }

    /// A child working in a folder, with `virtual_env` in its environment
    /// in lower case: the table gives its executable, its working folder
    /// and the prefix, found by its pid only.
    #[test]
    fn a_child_gives_its_folder_and_environment() {
        use std::io::BufRead as _;
        let d = tempfile::tempdir().unwrap();
        let work = d.path().join("work");
        std::fs::create_dir(&work).unwrap();
        let venv = d.path().join(".venv");
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "echo ready& set /p wait="])
            .current_dir(&work)
            .env_remove("VIRTUAL_ENV")
            .env_remove("CONDA_PREFIX")
            .env("virtual_env", &venv)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let mut line = String::new();
        io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        let found = current_user_processes().into_iter().find(|p| p.pid == pid);
        let _ = child.kill();
        let _ = child.wait();
        let info = found.expect("the child is the user's");
        assert!(info.name.eq_ignore_ascii_case("cmd.exe"), "{info:?}");
        assert!(same(info.cwd.as_deref().unwrap(), &canonical(&work)), "{info:?}");
        assert_eq!(info.env_prefixes.len(), 1, "{info:?}");
        assert!(same(&info.env_prefixes[0], &venv), "{info:?}");
        assert!(info.args.iter().any(|a| a == "/C"), "{info:?}");
        assert!(info.open_files.is_empty(), "a child of this process is not read for files: {info:?}");
    }

    /// A DLL loaded into the loader's list without running its entry point.
    struct Loaded(windows_sys::Win32::Foundation::HMODULE);

    impl Loaded {
        fn new(path: &Path) -> Self {
            use std::os::windows::ffi::OsStrExt as _;
            let wide: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
            // SAFETY: a NUL-terminated path; no file handle; the flag maps
            // the DLL into the loader's list and runs none of its code.
            let module = unsafe {
                windows_sys::Win32::System::LibraryLoader::LoadLibraryExW(
                    wide.as_ptr(),
                    std::ptr::null_mut(),
                    windows_sys::Win32::System::LibraryLoader::DONT_RESOLVE_DLL_REFERENCES,
                )
            };
            assert!(!module.is_null(), "{}: {}", path.display(), io::Error::last_os_error());
            Self(module)
        }
    }

    impl Drop for Loaded {
        fn drop(&mut self) {
            // SAFETY: the module LoadLibraryExW returned, freed once.
            unsafe { windows_sys::Win32::Foundation::FreeLibrary(self.0) };
        }
    }

    /// A system DLL copied to `to`.
    fn a_dll(to: &Path) {
        let mut dir = vec![0u16; 512];
        // SAFETY: `dir` is writable for its length in units.
        let len = unsafe {
            windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW(
                dir.as_mut_ptr(),
                u32::try_from(dir.len()).unwrap(),
            )
        } as usize;
        let system = PathBuf::from(std::ffi::OsString::from_wide(&dir[..len]));
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(system.join("version.dll"), to).unwrap();
    }

    fn holds(files: &[PathBuf], path: &Path) -> bool {
        files.iter().any(|f| same(f, path))
    }

    /// A module loaded from a folder is one of this process's open files,
    /// by its loader path; this process's own entry in the table reads no
    /// files, as on Linux.
    #[test]
    fn a_loaded_module_is_an_open_file() {
        let d = tempfile::tempdir().unwrap();
        let dll = canonical(d.path()).join("lib").join("ddc-test.dll");
        a_dll(&dll);
        let loaded = Loaded::new(&dll);
        let own = i32::try_from(std::process::id()).unwrap();
        let files = open_files(own);
        let me = current_user_processes().into_iter().find(|p| p.pid == own).unwrap();
        drop(loaded);
        assert!(holds(&files, &dll), "{}", files.iter().filter(|f| f.starts_with(d.path())).count());
        assert!(me.open_files.is_empty(), "this process is not read for files");
    }

    /// The system folder (`GetSystemDirectoryW`).
    fn system_dir() -> PathBuf {
        let mut dir = vec![0u16; 512];
        // SAFETY: `dir` is writable for its length in units.
        let len = unsafe {
            windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW(
                dir.as_mut_ptr(),
                u32::try_from(dir.len()).unwrap(),
            )
        } as usize;
        PathBuf::from(std::ffi::OsString::from_wide(&dir[..len]))
    }

    /// A process of the user that is neither ddc nor its child (a
    /// grandchild) is read for its modules: `ntdll.dll` among them, and
    /// nothing withheld.
    #[test]
    fn another_process_of_the_user_gives_its_modules() {
        let marker = format!("ddc-grandchild-{}", std::process::id());
        let inner = format!("echo ready& set /p wait=& rem {marker}");
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "cmd", "/C", &inner])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        ready(&mut child);
        let middle = i32::try_from(child.id()).unwrap();
        let table = current_user_processes();
        let grandchild =
            table.into_iter().find(|p| p.pid != middle && p.args.iter().any(|a| a.to_string_lossy().contains(&marker)));
        let _ = child.kill();
        let _ = child.wait();
        let info = grandchild.expect("the grandchild is the user's");
        let ntdll = system_dir().join("ntdll.dll");
        assert!(!info.withheld, "read in full ({} files)", info.open_files.len());
        assert!(holds(&info.open_files, &ntdll), "ntdll among {} files", info.open_files.len());
    }

    /// A module in a folder whose name the ANSI code page cannot spell is
    /// named exactly: Toolhelp's own path would carry `?` there.
    #[test]
    fn a_module_path_outside_the_ansi_code_page_is_exact() {
        let d = tempfile::tempdir().unwrap();
        let dll = canonical(d.path()).join("модули-日本").join("ddc-wide.dll");
        a_dll(&dll);
        let loaded = Loaded::new(&dll);
        let files = open_files(i32::try_from(std::process::id()).unwrap());
        drop(loaded);
        assert!(holds(&files, &dll), "the module is found");
        assert!(!files.iter().any(|f| f.to_string_lossy().contains('?')), "no name narrowed to ?");
    }

    /// A module loaded through a junction counts by both its loader path
    /// (through the link) and its mapped path (the file itself), so a
    /// store outside the finding is still seen (pnpm).
    #[test]
    fn a_module_loaded_through_a_junction_counts_by_both_paths() {
        let d = tempfile::tempdir().unwrap();
        let root = canonical(d.path());
        let store = root.join("store");
        a_dll(&store.join("ddc-junction.dll"));
        let link = root.join("link");
        let made = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(&link)
            .arg(&store)
            .output()
            .unwrap();
        assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
        let loaded = Loaded::new(&link.join("ddc-junction.dll"));
        let files = open_files(i32::try_from(std::process::id()).unwrap());
        drop(loaded);
        assert!(holds(&files, &link.join("ddc-junction.dll")), "the loader path");
        assert!(holds(&files, &store.join("ddc-junction.dll")), "the mapped path");
    }

    /// `ready`, read once, from a child that prints it when it runs.
    fn ready(child: &mut std::process::Child) {
        use std::io::BufRead as _;
        let mut line = String::new();
        io::BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "ready");
    }

    /// A variable set after the process started (an activation script in a
    /// running shell) moves its environment block: the prefix is still read.
    #[test]
    fn a_variable_set_after_the_start_is_read() {
        let d = tempfile::tempdir().unwrap();
        let venv = d.path().join(".venv");
        let script = format!("set VIRTUAL_ENV={}& echo ready& set /p wait=", venv.display());
        let mut child = std::process::Command::new("cmd")
            .args(["/C", &script])
            .env_remove("VIRTUAL_ENV")
            .env_remove("CONDA_PREFIX")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        ready(&mut child);
        let pid = i32::try_from(child.id()).unwrap();
        let found = current_user_processes().into_iter().find(|p| p.pid == pid);
        let _ = child.kill();
        let _ = child.wait();
        let info = found.expect("the child is the user's");
        assert!(info.env_prefixes.iter().any(|p| same(p, &venv)), "{info:?}");
    }

    /// A 32-bit process: its executable and command line are read, its
    /// working folder and environment are not (the 64-bit block holds their
    /// start), and it is withheld.
    #[test]
    fn a_32_bit_process_keeps_its_folder_and_environment_withheld() {
        let mut dir = vec![0u16; 512];
        // SAFETY: `dir` is writable for its length in units.
        let len = unsafe {
            windows_sys::Win32::System::SystemInformation::GetSystemWow64DirectoryW(
                dir.as_mut_ptr(),
                u32::try_from(dir.len()).unwrap(),
            )
        } as usize;
        if len == 0 || len >= dir.len() {
            return; // no 32-bit subsystem
        }
        let wow = PathBuf::from(std::ffi::OsString::from_wide(&dir[..len])).join("cmd.exe");
        if !wow.exists() {
            return;
        }
        let mut child = std::process::Command::new(&wow)
            .args(["/C", "echo ready& set /p wait="])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        ready(&mut child);
        let pid = i32::try_from(child.id()).unwrap();
        let found = current_user_processes().into_iter().find(|p| p.pid == pid);
        let _ = child.kill();
        let _ = child.wait();
        let info = found.expect("the child is the user's");
        assert!(info.withheld && info.cwd.is_none() && info.env_prefixes.is_empty(), "{info:?}");
        assert!(info.exe.as_deref().is_some_and(|e| same(e, &wow)) && !info.args.is_empty(), "{info:?}");
    }

    /// A SID from its parts, as Windows lays it out.
    fn sid(authority: u64, subs: &[u32]) -> Vec<u8> {
        let mut bytes = vec![1, u8::try_from(subs.len()).unwrap()];
        bytes.extend_from_slice(&authority.to_be_bytes()[2..]);
        for sub in subs {
            bytes.extend_from_slice(&sub.to_le_bytes());
        }
        bytes
    }

    const TRUSTED_INSTALLER: &str = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";

    #[test]
    fn the_root_is_own_when_its_owner_is_the_tokens() {
        let user = sid(5, &[21, 1, 2, 3, 1001]);
        let admins = sid(5, &[32, 544]);
        let system = sid(5, &[18]);
        assert_eq!(verdict(true, Some(&user), &user, &admins), RootOwner::Own, "the user's own");
        assert_eq!(verdict(true, Some(&admins), &user, &admins), RootOwner::Own, "the token's default owner");
        assert_eq!(verdict(true, Some(&system), &user, &admins), RootOwner::Other("S-1-5-18".to_owned()));
        assert_eq!(verdict(true, Some(&system), &user, &user), RootOwner::Other("S-1-5-18".to_owned()));
        assert_eq!(verdict(false, Some(&system), &user, &admins), RootOwner::Own, "not elevated");
        assert_eq!(verdict(true, None, &user, &admins), RootOwner::Own, "no owner kept");
    }

    #[test]
    fn local_system_is_only_s_1_5_18() {
        assert!(is_local_system(&sid(5, &[18])));
        assert!(!is_local_system(&sid(5, &[19])), "LocalService");
        assert!(!is_local_system(&sid(5, &[32, 544])));
        assert!(!is_local_system(&sid(5, &[21, 1, 2, 3, 18])));
    }

    #[test]
    fn a_sid_prints_as_windows_prints_it() {
        assert_eq!(sid_string(&sid(5, &[18])), "S-1-5-18");
        assert_eq!(sid_string(&sid(5, &[32, 544])), "S-1-5-32-544");
        assert_eq!(sid_string(&sid(1, &[0])), "S-1-1-0", "Everyone");
        assert_eq!(
            sid_string(&sid(5, &[80, 956008885, 3418522649, 1831038044, 1853292631, 2271478464])),
            TRUSTED_INSTALLER
        );
        assert_eq!(sid_string(&sid(5, &[21, 1, 2, 3, 1001])), "S-1-5-21-1-2-3-1001");
        assert_eq!(sid_string(&sid(1 << 40, &[7])), "S-1-0x010000000000-7", "a wide authority in hex");
        assert_eq!(sid_string(&[1, 2, 0, 0, 0, 0, 0, 5]), "0102000000000005", "too short for its count");
        assert_eq!(sid_string(&[]), "");
    }

    #[test]
    fn this_session_is_not_local_system() {
        assert!(!is_root());
    }

    #[test]
    fn a_temporary_folder_is_the_users_own() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(root_owner(d.path()).unwrap(), RootOwner::Own);
    }

    fn windows_folder() -> PathBuf {
        use windows_sys::Win32::System::SystemInformation::GetSystemWindowsDirectoryW;
        let mut buf = [0u16; 512];
        // SAFETY: `buf` is writable for its length, given in characters.
        let n = unsafe { GetSystemWindowsDirectoryW(buf.as_mut_ptr(), buf.len() as u32) } as usize;
        assert!(n > 0 && n < buf.len(), "{}", io::Error::last_os_error());
        PathBuf::from(String::from_utf16(&buf[..n]).unwrap())
    }

    /// The Windows folder belongs to TrustedInstaller: refused to an
    /// elevated token, the user's to scan otherwise.
    #[test]
    fn the_windows_folder_is_refused_only_when_elevated() {
        let expected =
            if token().unwrap().elevated { RootOwner::Other(TRUSTED_INSTALLER.to_owned()) } else { RootOwner::Own };
        assert_eq!(root_owner(&windows_folder()).unwrap(), expected);
    }

    /// The privileges that pass every ACL are gone from the token once it
    /// is read, whatever the session started with (an elevated OpenSSH
    /// session has them enabled).
    #[test]
    fn the_token_holds_no_privilege_that_passes_every_acl() {
        token().unwrap();
        assert_eq!(acl_bypass(&open_token().unwrap()).unwrap(), [None; 3]);
        drop_acl_bypass_privileges().unwrap();
    }
}
