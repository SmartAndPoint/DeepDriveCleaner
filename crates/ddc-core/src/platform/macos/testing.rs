//! What in-source tests need from the OS, so a test module names no
//! `std::os` path of its own.

use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};

/// A symlink at `link` to `target`.
pub fn symlink(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

/// `bytes` as a path name, valid UTF-8 or not; `None` where the platform
/// cannot spell arbitrary bytes.
pub fn name_from_bytes(bytes: &[u8]) -> Option<OsString> {
    Some(OsString::from_vec(bytes.to_vec()))
}

/// `Ok`: paths here are POSIX-shaped, so a test that spells `/Users/…` or
/// `/usr/bin/…` as absolute runs.
pub fn posix_paths() -> io::Result<()> {
    Ok(())
}

/// A command that runs for about `secs` seconds and then succeeds.
pub fn sleep_command(secs: u32) -> (PathBuf, Vec<String>) {
    (PathBuf::from("/bin/sleep"), vec![secs.to_string()])
}

/// A command that prints the file `path` as it is, bytes not UTF-8 included.
pub fn print_file_command(path: &Path) -> (PathBuf, Vec<String>) {
    (PathBuf::from("/bin/cat"), vec![path.to_string_lossy().into_owned()])
}
