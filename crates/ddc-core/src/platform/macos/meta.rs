//! What a `std::fs::Metadata` holds beyond std's portable accessors, on
//! macOS: identity, links, allocation, times, mode bits and `st_flags`.

use std::fs::{Metadata, Permissions};
use std::io;
use std::os::macos::fs::MetadataExt as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

/// `(dev, ino)`, as `stat` gives them and `bulk` widens them, so the pair
/// compares with a directory listing's. Never fails on macOS; an OS module
/// that cannot tell a file's identity returns an error, never a made-up pair.
pub fn file_id(md: &Metadata) -> io::Result<(u64, u64)> {
    Ok((md.dev(), md.ino()))
}

/// Hard links to the file (`st_nlink`).
pub fn link_count(md: &Metadata) -> u64 {
    md.nlink()
}

/// Bytes allocated on disk, `st_blocks * 512`. Not `ATTR_FILE_ALLOCSIZE`,
/// which `bulk` reads: the two stay distinct.
pub fn alloc_bytes(md: &Metadata) -> u64 {
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

/// The owner's user id: not reported on macOS yet, so nothing there is
/// refused for its owner (safety ADR, amendment of 2026-10-31).
pub fn owner(_md: &Metadata) -> Option<u32> {
    None
}

/// Adds `u+rwx`.
pub fn add_owner_rwx(perms: &mut Permissions) {
    perms.set_mode(perms.mode() | 0o700);
}

/// Whether someone may execute it: any of the `x` bits.
pub fn is_executable(md: &Metadata) -> bool {
    md.permissions().mode() & 0o111 != 0
}
