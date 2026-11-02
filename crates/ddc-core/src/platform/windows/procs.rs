//! Processes and this process's token, on Windows.
//!
//! The in-use checks are not ported yet: they are empty, which fails open.
//! They are never reached: `require_supported` refuses in `execute`,
//! `preflight` and `free::run`, and the CLI refuses at dispatch before them.
//! The port fills them before it sets `SUPPORTED`.
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

use windows_sys::Win32::Foundation::{ERROR_SUCCESS, HANDLE, LUID, LocalFree};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    AdjustTokenPrivileges, GetLengthSid, GetTokenInformation, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_BACKUP_NAME, SE_PRIVILEGE_REMOVED, SE_RESTORE_NAME,
    SE_TAKE_OWNERSHIP_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_ELEVATION, TOKEN_INFORMATION_CLASS, TOKEN_OWNER,
    TOKEN_PRIVILEGES, TOKEN_QUERY, TOKEN_USER, TokenElevation, TokenOwner, TokenPrivileges, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, READ_CONTROL};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

pub use crate::platform::types::{OpenVnode, ProcInfo, ProcStart, RootOwner, TokenUnusable, app_bundle};

/// No user's id: never compared, since nothing here owns a process.
pub fn uid() -> u32 {
    u32::MAX
}

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

pub fn current_user_processes() -> Vec<ProcInfo> {
    Vec::new()
}

pub fn current_user_processes_unopened() -> Vec<ProcInfo> {
    Vec::new()
}

pub fn open_files(_pid: i32) -> Vec<PathBuf> {
    Vec::new()
}

pub fn open_vnodes(_pid: i32) -> Vec<OpenVnode> {
    Vec::new()
}

pub fn process_start(_pid: i32) -> Option<ProcStart> {
    None
}

pub fn thread_count(_pid: i32) -> Option<u32> {
    None
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
