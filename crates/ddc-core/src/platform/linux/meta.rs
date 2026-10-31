//! What a `std::fs::Metadata` holds beyond std's portable accessors, on
//! Linux: identity, links, allocation, times and mode bits. Linux has no
//! `st_flags`.

use std::fs::{Metadata, Permissions};
use std::io;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

/// `(dev, ino)`, as `stat` gives them, so the pair compares with a
/// directory listing's.
pub fn file_id(md: &Metadata) -> io::Result<(u64, u64)> {
    Ok((md.dev(), md.ino()))
}

/// Hard links to the file (`st_nlink`).
pub fn link_count(md: &Metadata) -> u64 {
    md.nlink()
}

/// Bytes allocated on disk, `st_blocks * 512`.
pub fn alloc_bytes(md: &Metadata) -> u64 {
    md.blocks() * 512
}

/// Modification time, Unix seconds (`st_mtime`).
pub fn mtime(md: &Metadata) -> i64 {
    md.mtime()
}

/// Always 0: Linux has no `st_flags`, so nothing is dataless.
pub fn st_flags(_md: &Metadata) -> u32 {
    0
}

/// The owner's user id (`st_uid`).
pub fn owner(md: &Metadata) -> Option<u32> {
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
