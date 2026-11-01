//! What a `std::fs::Metadata` holds beyond std's portable accessors, on
//! macOS: identity, links, allocation, times, mode bits and `st_flags`.

use std::fs::{Metadata, Permissions};

use crate::platform::types::FileStat;
use std::io;
use std::os::macos::fs::MetadataExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;

/// `(dev, ino)`, as `stat` gives them and `bulk` widens them, so the pair
/// compares with a directory listing's. Never fails on macOS; an OS module
/// that cannot tell a file's identity returns an error, never a made-up pair.
pub(super) fn file_id(md: &Metadata) -> io::Result<(u64, u64)> {
    Ok((md.dev(), md.ino()))
}

/// Hard links to the file (`st_nlink`).
pub(super) fn link_count(md: &Metadata) -> u64 {
    md.nlink()
}

/// Bytes allocated on disk, `st_blocks * 512`. Not `ATTR_FILE_ALLOCSIZE`,
/// which `bulk` reads: the two stay distinct.
pub(super) fn alloc_bytes(md: &Metadata) -> u64 {
    md.blocks() * 512
}

/// Modification time, Unix seconds (`st_mtime`).
pub fn mtime(md: &Metadata) -> i64 {
    md.mtime()
}

/// `st_flags` (`SF_DATALESS`, …).
pub fn st_flags(md: &Metadata) -> u32 {
    md.st_flags()
}

/// The owner's user id for the removal guard and the scan's findings: not
/// reported on macOS yet, so nothing there is refused for its owner (safety
/// ADR, amendment of 2026-10-31). It stays `None` here; [`uid`] is not a
/// way around that.
pub fn owner(_md: &Metadata) -> Option<u32> {
    None
}

/// The owner's user id (`st_uid`), only as a read gate: the agents report
/// opens Claude Code's temporary folder only when the user owns it. Never
/// fed to the removal guard or a finding, which use [`owner`] (`None` on
/// macOS, safety ADR, amendment of 2026-10-31).
pub fn uid(md: &Metadata) -> Option<u32> {
    Some(md.uid())
}

/// Adds `u+rwx`.
pub fn add_owner_rwx(perms: &mut Permissions) {
    perms.set_mode(perms.mode() | 0o700);
}

/// Whether someone may execute it: any of the `x` bits.
pub fn is_executable(md: &Metadata) -> bool {
    md.permissions().mode() & 0o111 != 0
}

/// The path's own facts, links not followed (`lstat`).
pub fn lstat(path: &Path) -> io::Result<FileStat> {
    std::fs::symlink_metadata(path).and_then(of)
}

/// The facts of what the path names, links followed (`stat`).
pub fn stat(path: &Path) -> io::Result<FileStat> {
    std::fs::metadata(path).and_then(of)
}

/// A listed entry's facts, links not followed, read through the listing
/// (`fstatat` on its directory), so it reaches past `PATH_MAX`.
pub fn entry(entry: &std::fs::DirEntry) -> io::Result<FileStat> {
    entry.metadata().and_then(of)
}

fn of(md: Metadata) -> io::Result<FileStat> {
    let (dev, ino) = file_id(&md)?;
    Ok(FileStat { dev, ino, nlink: link_count(&md), alloc: alloc_bytes(&md), md })
}
