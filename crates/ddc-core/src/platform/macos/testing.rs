//! What in-source tests need from the OS, so a test module names no
//! `std::os` path of its own.

use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStringExt as _;
use std::path::Path;

/// A symlink at `link` to `target`.
pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// `bytes` as a path name, valid UTF-8 or not; `None` where the platform
/// cannot spell arbitrary bytes.
pub fn name_from_bytes(bytes: &[u8]) -> Option<OsString> {
    Some(OsString::from_vec(bytes.to_vec()))
}
