//! What in-source tests need from the OS, so a test module names no
//! `std::os` path of its own.

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt as _;
use std::path::Path;

/// A symlink at `link` to `target`: a directory link when the target is a
/// folder, as Windows keeps the two apart. Needs the symlink privilege
/// (Developer Mode or an elevated token); without it the error is
/// `ERROR_PRIVILEGE_NOT_HELD` and the test skips.
pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    let resolved = if target.is_absolute() { target.to_path_buf() } else { link.with_file_name(target) };
    if resolved.is_dir() {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

/// A name that is not valid UTF-8 on Windows: `bytes`' first byte as an
/// unpaired surrogate (WTF-8), which NTFS stores but no `String` holds.
/// Valid UTF-8 `bytes` come back as they are.
pub fn name_from_bytes(bytes: &[u8]) -> Option<OsString> {
    match std::str::from_utf8(bytes) {
        Ok(s) => Some(OsString::from(s)),
        Err(_) => {
            let mut wide: Vec<u16> = String::from_utf8_lossy(bytes).replace('\u{fffd}', "").encode_utf16().collect();
            wide.push(0xD800);
            Some(OsString::from_wide(&wide))
        }
    }
}

/// Not POSIX-shaped: Windows reads `/Users/…` as relative to the current
/// drive, so a test that spells it as absolute skips.
pub fn posix_paths() -> io::Result<()> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "paths here are not POSIX-shaped"))
}
