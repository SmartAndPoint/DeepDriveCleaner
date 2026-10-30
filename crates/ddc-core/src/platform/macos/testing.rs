//! What in-source tests need from the OS, so a test module names no
//! `std::os` path of its own.

use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStringExt as _;
use std::path::Path;
use std::process::Command;

/// A symlink at `link` to `target`.
pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// `bytes` as a path name, valid UTF-8 or not; `None` where the platform
/// cannot spell arbitrary bytes.
pub fn name_from_bytes(bytes: &[u8]) -> Option<OsString> {
    Some(OsString::from_vec(bytes.to_vec()))
}

/// Converts the property list at `path` to the binary format, in place,
/// with `/usr/bin/plutil`.
pub fn plutil_to_binary(path: &Path) -> io::Result<()> {
    let status = Command::new("/usr/bin/plutil").args(["-convert", "binary1"]).arg(path).status()?;
    if status.success() { Ok(()) } else { Err(io::Error::other(format!("plutil -convert binary1: {status}"))) }
}
