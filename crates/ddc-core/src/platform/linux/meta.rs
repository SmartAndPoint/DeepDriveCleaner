//! What a `std::fs::Metadata` holds beyond std's portable accessors, on
//! Linux: identity, links, allocation, times and mode bits. Linux has no
//! `st_flags`.

use std::fs::{Metadata, Permissions};

use crate::platform::types::FileStat;
use std::io;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::Path;

/// Hard links to the file (`st_nlink`).
pub(super) fn link_count(md: &Metadata) -> u64 {
    md.nlink()
}

/// Bytes allocated on disk, `st_blocks * 512`.
pub(super) fn alloc_bytes(md: &Metadata) -> u64 {
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

/// The owner's user id (`st_uid`), for the removal guard and the scan's
/// findings; [`uid`] is the agents report's read gate.
pub fn owner(md: &Metadata) -> Option<u32> {
    Some(md.uid())
}

/// The owner's user id (`st_uid`), only as a read gate: the agents report
/// opens Claude Code's temporary folder only when the user owns it. The
/// same value as [`owner`] here, but never fed to the removal guard or a
/// finding, which use [`owner`].
pub fn uid(md: &Metadata) -> Option<u32> {
    Some(md.uid())
}

/// Adds `u+rwx`.
pub fn add_owner_rwx(perms: &mut Permissions) {
    perms.set_mode(perms.mode() | 0o700);
}

/// Whether someone may execute it: any of the `x` bits.
pub fn is_executable(_path: &Path, md: &Metadata) -> bool {
    md.permissions().mode() & 0o111 != 0
}

/// The path's own facts, links not followed (`lstat`). The device is the
/// one every producer reports (`super::fsid`), read through the path
/// opened again and checked to be the same file.
pub fn lstat(path: &Path) -> io::Result<FileStat> {
    let md = std::fs::symlink_metadata(path)?;
    let dev = super::fsid::of_path(path, false, &md)?;
    Ok(of(md, dev))
}

/// The facts of what the path names, links followed (`stat`), its device
/// as [`lstat`]'s.
pub fn stat(path: &Path) -> io::Result<FileStat> {
    let md = std::fs::metadata(path)?;
    let dev = super::fsid::of_path(path, true, &md)?;
    Ok(of(md, dev))
}

/// A listed entry's facts, links not followed, read through the listing:
/// std's `DirEntry::metadata`, which is `fstatat` on the listing's
/// descriptor (glibc and macOS) and reaches past `PATH_MAX`; on musl std
/// stats the path instead. The device as [`lstat`]'s, through the
/// entry's folder past `PATH_MAX`.
pub fn entry(entry: &std::fs::DirEntry) -> io::Result<FileStat> {
    let md = entry.metadata()?;
    let dev = super::fsid::of_path(&entry.path(), false, &md)?;
    Ok(of(md, dev))
}

fn of(md: Metadata, dev: u64) -> FileStat {
    FileStat { dev, ino: md.ino(), nlink: link_count(&md), alloc: alloc_bytes(&md), md }
}
