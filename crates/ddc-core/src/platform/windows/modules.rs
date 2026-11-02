//! The modules another process has loaded, as its open files, on Windows
//! (safety ADR §5, amendment of 2026-11-02, the process check; story
//! "Windows scanner and rule catalog", step 9b). A DLL, `.pyd` or `.node`
//! loaded from inside a finding lets the tombstone rename pass, as a
//! running executable does, and `remove_dir_all` would half-delete it: the
//! process check is the only defence.
//!
//! - The list is a Toolhelp module snapshot (`TH32CS_SNAPMODULE |
//!   TH32CS_SNAPMODULE32`), retried a bounded number of times while the
//!   process changes its modules (`ERROR_BAD_LENGTH`) or is starting
//!   (`ERROR_PARTIAL_COPY`).
//! - Each module counts by two paths. Its loader path is the name it was
//!   loaded by, with no link resolved: a DLL loaded through a junction from
//!   a store outside the finding (pnpm) does not name the store. Its mapped
//!   file's path (`K32GetMappedFileNameW`) is the file itself, in its device
//!   form, turned into a path by [`Devices`]. A loader path that fills
//!   Toolhelp's `MAX_PATH` field is read again in full
//!   (`K32GetModuleFileNameExW`).
//! - A module list that cannot be read is `None`: the caller sets
//!   `withheld`.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{ERROR_BAD_LENGTH, ERROR_NO_MORE_FILES, ERROR_PARTIAL_COPY, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32,
};
use windows_sys::Win32::System::ProcessStatus::{K32GetMappedFileNameW, K32GetModuleFileNameExW};

/// How many times a snapshot is taken again while it is refused for a
/// passing reason.
const ATTEMPTS: u32 = 3;
/// The longest name Windows has, in UTF-16 units.
const NAME_MAX: usize = 32_767;

/// The volumes' NT devices and where each is mounted, read once per table.
pub(super) struct Devices(Vec<(String, PathBuf)>);

impl Devices {
    pub(super) fn read() -> Self {
        Self(
            super::statfs::devices()
                .into_iter()
                .map(|(device, point)| (String::from_utf16_lossy(&device), point))
                .collect(),
        )
    }

    /// `\Device\HarddiskVolume3\x\y.dll` as `C:\x\y.dll`, once for each
    /// place that volume is mounted at; none for a device no volume of the
    /// table is, a share (`\Device\Mup`) among them.
    fn paths(&self, device_path: &[u16]) -> Vec<PathBuf> {
        let Ok(text) = String::from_utf16(device_path) else { return Vec::new() };
        self.0
            .iter()
            .filter_map(|(device, point)| {
                // A device name is ASCII: compared without case, by its bytes.
                let head = text.get(..device.len())?;
                let rest = head.eq_ignore_ascii_case(device).then(|| &text[device.len()..])?.strip_prefix('\\')?;
                Some(point.join(rest))
            })
            .collect()
    }
}

/// The modules a process has loaded, and whether every one of them was read.
pub(super) struct Modules {
    pub files: Vec<PathBuf>,
    pub complete: bool,
}

/// The paths of the modules `pid` has loaded, through the handle `handle`
/// (open with `PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ`). A
/// module whose names cannot be read is left out and the list marked
/// incomplete, so one module unloaded meanwhile never hides the others;
/// `None` when no list could be taken at all.
pub(super) fn of(pid: u32, handle: &OwnedHandle, devices: &Devices) -> Option<Modules> {
    let snap = snapshot(pid)?;
    let mut entry = MODULEENTRY32W { dwSize: u32::try_from(size_of::<MODULEENTRY32W>()).ok()?, ..Default::default() };
    let mut out = Modules { files: Vec::new(), complete: true };
    let mut seen = HashSet::new();
    let mut add = |files: &mut Vec<PathBuf>, path: PathBuf| {
        let path = super::paths::simplified(path);
        if seen.insert(super::paths::key(&path).into_owned()) {
            files.push(path);
        }
    };
    // One buffer for every name of this process.
    let mut buf = vec![0u16; NAME_MAX + 1];
    // SAFETY: the snapshot is open; `entry` is a local with its size set.
    let mut more = unsafe { Module32FirstW(snap.as_raw_handle(), &mut entry) } != 0;
    if !more {
        // A live process has at least its image and ntdll.
        return None;
    }
    while more {
        // Toolhelp's own path is cut short and narrowed to the ANSI code
        // page: the loader is asked for the whole name.
        match full_name(handle, entry.hModule, &mut buf) {
            Some(loader) => add(&mut out.files, loader),
            None => out.complete = false,
        }
        match mapped_name(handle, entry.modBaseAddr.cast(), &mut buf) {
            Some(mapped) => {
                for path in devices.paths(&mapped) {
                    add(&mut out.files, path);
                }
            }
            None => out.complete = false,
        }
        // SAFETY: as for Module32FirstW.
        more = unsafe { Module32NextW(snap.as_raw_handle(), &mut entry) } != 0;
    }
    if io::Error::last_os_error().raw_os_error() != Some(ERROR_NO_MORE_FILES as i32) {
        out.complete = false;
    }
    Some(out)
}

/// The module snapshot of `pid`, taken again while it is refused for a
/// passing reason.
fn snapshot(pid: u32) -> Option<OwnedHandle> {
    for _ in 0..ATTEMPTS {
        // SAFETY: plain arguments; the handle is checked below.
        let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid) };
        if raw != INVALID_HANDLE_VALUE {
            // SAFETY: a handle CreateToolhelp32Snapshot just returned, owned by nobody else.
            return Some(unsafe { OwnedHandle::from_raw_handle(raw) });
        }
        let code = io::Error::last_os_error().raw_os_error();
        if code != Some(ERROR_BAD_LENGTH as i32) && code != Some(ERROR_PARTIAL_COPY as i32) {
            return None;
        }
    }
    None
}

/// The whole loader path of `module`.
fn full_name(
    handle: &OwnedHandle,
    module: windows_sys::Win32::Foundation::HMODULE,
    buf: &mut [u16],
) -> Option<PathBuf> {
    // SAFETY: the handle is open for the call; `buf` is writable for its length.
    let len = unsafe {
        K32GetModuleFileNameExW(handle.as_raw_handle(), module, buf.as_mut_ptr(), u32::try_from(buf.len()).ok()?)
    } as usize;
    (len > 0 && len < buf.len()).then(|| PathBuf::from(OsString::from_wide(&buf[..len])))
}

/// The device path of the file mapped at `base` in the other process.
fn mapped_name(handle: &OwnedHandle, base: *const core::ffi::c_void, buf: &mut [u16]) -> Option<Vec<u16>> {
    // SAFETY: the handle is open for the call; `base` is an address in the
    // other process, which the call checks; `buf` is writable for its length.
    let len = unsafe {
        K32GetMappedFileNameW(handle.as_raw_handle(), base, buf.as_mut_ptr(), u32::try_from(buf.len()).ok()?)
    } as usize;
    (len > 0 && len < buf.len()).then(|| buf[..len].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn a_device_path_is_turned_into_its_mount_point() {
        let devices = Devices(vec![
            (r"\DEVICE\HARDDISKVOLUME3".to_owned(), PathBuf::from(r"C:\")),
            (r"\DEVICE\HARDDISKVOLUME7".to_owned(), PathBuf::from(r"D:\")),
            (r"\DEVICE\HARDDISKVOLUME7".to_owned(), PathBuf::from(r"C:\mnt\dev\")),
        ]);
        assert_eq!(devices.paths(&wide(r"\Device\HarddiskVolume3\x\Y.dll")), [PathBuf::from(r"C:\x\Y.dll")]);
        assert_eq!(
            devices.paths(&wide(r"\Device\HarddiskVolume7\n\a.node")),
            [PathBuf::from(r"D:\n\a.node"), PathBuf::from(r"C:\mnt\dev\n\a.node")],
            "every place the volume is mounted at"
        );
        assert!(devices.paths(&wide(r"\Device\HarddiskVolume33\x.dll")).is_empty(), "another volume, not a prefix");
        assert!(devices.paths(&wide(r"\Device\Mup\server\share\x.dll")).is_empty(), "a share");
    }
}
