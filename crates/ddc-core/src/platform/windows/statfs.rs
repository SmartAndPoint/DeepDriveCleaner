//! Volume statistics on Windows (story "Windows scanner and rule catalog",
//! step 7): the volume a path lies on (`GetVolumePathNameW`), its sizes
//! (`GetDiskFreeSpaceExW`), its file system's name (`GetVolumeInformationW`)
//! and its volume name (`\\?\Volume{…}\`). A volume can be mounted at a
//! drive letter and at folders; [`mounts`] lists each mount point, spelled
//! as [`statfs`] spells it (`C:\`, `C:\mnt\data`). Unlike Unix, [`statfs`]
//! answers for a path that does not exist on an existing volume (Windows
//! finds the volume from the path's leading part); no caller needs the
//! error. Under a per-user quota `total_bytes` is the quota, and
//! `free_bytes` (the volume's) can be larger.

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};

use windows_sys::Win32::Foundation::{ERROR_MORE_DATA, ERROR_NO_MORE_FILES, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Storage::FileSystem::{
    FindFirstVolumeW, FindNextVolumeW, FindVolumeClose, GetDiskFreeSpaceExW, GetVolumeInformationW,
    GetVolumeNameForVolumeMountPointW, GetVolumePathNameW, GetVolumePathNamesForVolumeNameW, QueryDosDeviceW,
};

pub use crate::platform::types::VolumeStats;

/// Room for a volume name, `\\?\Volume{GUID}\`, and for a mount point.
const NAME: usize = 1024;

pub fn statfs(path: &Path) -> io::Result<VolumeStats> {
    let wide = super::wide::verbatim(path)?;
    // At least as long as the path, as the call needs (a finding's path can
    // be far past MAX_PATH).
    let len = wide.len().max(NAME);
    let mut point = vec![0u16; len];
    // SAFETY: a NUL-terminated wide path; the output buffer is writable for
    // its stated length.
    if unsafe { GetVolumePathNameW(wide.as_ptr(), point.as_mut_ptr(), u32::try_from(len).unwrap_or(u32::MAX)) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let point = until_nul(&point);
    let mut volume = vec![0u16; NAME];
    let mut nul = point.clone();
    nul.push(0);
    // SAFETY: as above. A mount point that is no volume's (a network share)
    // fails; its volume name is then left empty.
    let named = unsafe { GetVolumeNameForVolumeMountPointW(nul.as_ptr(), volume.as_mut_ptr(), NAME as u32) } != 0;
    let volume = if named { until_nul(&volume) } else { Vec::new() };
    stats(&point, &volume)
}

/// Every volume at each of its mount points; a volume with none, or one the
/// user cannot query (an empty card reader), is left out.
/// Not told on Windows yet, whose builds have no slot (safety ADR §5,
/// amendment of 2026-11-02): every file system counts as network, so a
/// caller that needs a local home fails closed.
pub fn is_network_fs(_fs_type: &str) -> bool {
    true
}

pub fn mounts() -> io::Result<Vec<VolumeStats>> {
    let mut out = Vec::new();
    let mut volume = vec![0u16; NAME];
    // SAFETY: the buffer is writable for its stated length.
    let find = unsafe { FindFirstVolumeW(volume.as_mut_ptr(), NAME as u32) };
    if find == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let _close = Search(find);
    loop {
        let name = until_nul(&volume);
        for point in mount_points(&name) {
            if let Ok(s) = stats(&point, &name) {
                out.push(s);
            }
        }
        // SAFETY: `find` is the open search handle; the buffer as above.
        if unsafe { FindNextVolumeW(find, volume.as_mut_ptr(), NAME as u32) } == 0 {
            let e = io::Error::last_os_error();
            return if e.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) { Ok(out) } else { Err(e) };
        }
    }
}

/// Every volume's NT device (`\Device\HarddiskVolume3`) with each of its
/// mount points (`C:\`, and a folder, or only a folder for a volume
/// without a letter), for turning a mapped file's device path into paths
/// (`modules`). A volume mounted nowhere is left out.
pub(super) fn devices() -> Vec<(Vec<u16>, PathBuf)> {
    let mut out = Vec::new();
    let mut volume = vec![0u16; NAME];
    // SAFETY: the buffer is writable for its stated length.
    let find = unsafe { FindFirstVolumeW(volume.as_mut_ptr(), NAME as u32) };
    if find == INVALID_HANDLE_VALUE {
        return out;
    }
    let _close = Search(find);
    loop {
        let name = until_nul(&volume);
        if let Some(device) = device_of(&name) {
            for point in mount_points(&name) {
                out.push((device.clone(), PathBuf::from(OsString::from_wide(&point))));
            }
        }
        // SAFETY: `find` is the open search handle; the buffer as above.
        if unsafe { FindNextVolumeW(find, volume.as_mut_ptr(), NAME as u32) } == 0 {
            return out;
        }
    }
}

/// The NT device of the volume `name` (`\\?\Volume{…}\`), from
/// `QueryDosDeviceW` of `Volume{…}`.
fn device_of(name: &[u16]) -> Option<Vec<u16>> {
    let text = String::from_utf16(name).ok()?;
    let guid = text.strip_prefix(r"\\?\")?.strip_suffix('\\')?;
    let mut query: Vec<u16> = guid.encode_utf16().collect();
    query.push(0);
    let mut target = vec![0u16; NAME];
    // SAFETY: a NUL-terminated device name; the buffer is writable for its
    // stated length.
    let len = unsafe { QueryDosDeviceW(query.as_ptr(), target.as_mut_ptr(), NAME as u32) } as usize;
    // The answer is a list of NUL-terminated names; the first is the device.
    (len > 0).then(|| until_nul(&target)).filter(|d| !d.is_empty())
}

/// A volume search, closed once whichever way [`mounts`] leaves.
struct Search(windows_sys::Win32::Foundation::HANDLE);

impl Drop for Search {
    fn drop(&mut self) {
        // SAFETY: the open search handle, closed here only.
        unsafe { FindVolumeClose(self.0) };
    }
}

/// The mount points of the volume `name` (`\\?\Volume{…}\`), each ending in
/// `\`: its drive letter and the folders it is mounted at.
fn mount_points(name: &[u16]) -> Vec<Vec<u16>> {
    let mut volume = name.to_vec();
    volume.push(0);
    let mut buffer = vec![0u16; NAME];
    loop {
        let mut needed = 0u32;
        // SAFETY: a NUL-terminated volume name; the buffer is writable for
        // its stated length; `needed` is a writable local.
        let ok = unsafe {
            GetVolumePathNamesForVolumeNameW(
                volume.as_ptr(),
                buffer.as_mut_ptr(),
                u32::try_from(buffer.len()).unwrap_or(u32::MAX),
                &mut needed,
            )
        };
        if ok != 0 {
            // A list of NUL-terminated names, ended by an empty one.
            return buffer.split(|&u| u == 0).take_while(|s| !s.is_empty()).map(<[u16]>::to_vec).collect();
        }
        if io::Error::last_os_error().raw_os_error() != Some(ERROR_MORE_DATA as i32) {
            return Vec::new();
        }
        buffer.resize(usize::try_from(needed).unwrap_or(0).max(buffer.len() + 1), 0);
    }
}

/// The sizes and file system of the volume mounted at `point` (ending in
/// `\`), whose volume name is `volume`.
fn stats(point: &[u16], volume: &[u16]) -> io::Result<VolumeStats> {
    let mut root = point.to_vec();
    root.push(0);
    let (mut available, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: a NUL-terminated root; three writable locals.
    if unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut available, &mut total, &mut free) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut fs = vec![0u16; 261];
    // SAFETY: a NUL-terminated root; no volume label, serial, length or flags
    // asked for (null); the file system name buffer is writable for its
    // stated length.
    let ok = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    };
    let fs_type = if ok != 0 { String::from_utf16_lossy(&until_nul(&fs)).to_lowercase() } else { String::new() };
    Ok(VolumeStats {
        fs_type,
        mount_point: spelled(point),
        available_bytes: available,
        total_bytes: total,
        free_bytes: free,
        mounted_from: String::from_utf16_lossy(volume),
    })
}

/// A mount point as both functions report it: `C:\`, not the `\\?\C:\` a
/// verbatim path's volume comes back as, and a folder without its trailing
/// `\` (`C:\mnt\data`), as Unix spells a mount point.
fn spelled(point: &[u16]) -> PathBuf {
    let plain = super::paths::simplified(PathBuf::from(OsString::from_wide(point)));
    plain.components().collect()
}

fn until_nul(buffer: &[u16]) -> Vec<u16> {
    buffer.iter().take_while(|&&u| u != 0).copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_temp_folder_s_volume_has_sizes_and_a_name() {
        let temp = std::env::temp_dir();
        let s = statfs(&temp).unwrap();
        assert!(s.mount_point.as_os_str().to_string_lossy().ends_with('\\'), "{}", s.mount_point.display());
        assert!(s.total_bytes > 0 && s.available_bytes <= s.free_bytes, "{s:?}");
        assert!(!s.fs_type.is_empty(), "the file system's name");
        assert!(s.mounted_from.starts_with(r"\\?\Volume{"), "{}", s.mounted_from);
        let all = mounts().unwrap();
        assert!(all.iter().any(|m| m.mount_point == s.mount_point && m.total_bytes == s.total_bytes), "{all:?}");
    }

    /// A path far past MAX_PATH (the tail need not exist) finds its volume.
    #[test]
    fn a_long_path_finds_its_volume() {
        let temp = std::env::temp_dir();
        let long = temp
            .join("x".repeat(200))
            .join("y".repeat(200))
            .join("z".repeat(200))
            .join("w".repeat(200))
            .join("v".repeat(200))
            .join("u".repeat(200));
        assert!(long.as_os_str().len() > 1100);
        assert_eq!(statfs(&long).unwrap().mount_point, statfs(&temp).unwrap().mount_point);
    }

    /// The same volume through the local administrative share: the mount
    /// point is the share's root, no volume name. Skips where the share is
    /// not there (it needs an administrator's session).
    #[test]
    fn a_share_s_root_is_its_mount_point() {
        let temp = statfs(&std::env::temp_dir()).unwrap();
        let drive = temp.mount_point.to_string_lossy().chars().next().unwrap();
        let share = PathBuf::from(format!(r"\\localhost\{drive}$\"));
        let Ok(s) = statfs(&share) else { return };
        assert_eq!(s.mount_point, share.components().collect::<PathBuf>(), "{s:?}");
        assert!(s.mounted_from.is_empty() && s.total_bytes > 0, "{s:?}");
    }

    #[test]
    fn a_drive_with_nothing_behind_it_is_an_error() {
        // SAFETY: no arguments; a bitmask of the assigned letters.
        let used = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
        let free = (b'D'..=b'Z').rev().find(|d| used & (1 << (d - b'A')) == 0).map(char::from).unwrap();
        assert!(statfs(Path::new(&format!("{free}:\\x"))).is_err());
    }
}
