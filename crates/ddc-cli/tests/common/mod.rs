//! What the CLI tests share across platforms (ADR "Platform seam: one
//! platform module per OS with declared capabilities", §9): the skip of a
//! command the build refuses, and the Unix-only helpers, which keep one
//! signature on every OS and panic elsewhere, where only a test that has
//! already skipped reaches them.
#![allow(dead_code)]

use std::fs;
use std::path::Path;

/// A test that runs `scan`, `clean`, `du`, `disk`, `schedule`, `notify` or
/// `diag`, which a build with `SUPPORTED` false refuses, starts with
/// `if common::unsupported("name") { return; }`.
pub use ddc_loadgen::unsupported;

/// A test of a command or action that needs a capability (`ddc disk`, `ddc
/// disk agents`, Homebrew) starts with
/// `if common::lacks("name", Capability::WholeDisk) { return; }`.
#[allow(unused_imports)] // only the capability tests use it
pub use ddc_loadgen::lacks;

/// `(dev, ino)` of one path, without following symlinks.
pub fn id(path: impl AsRef<Path>) -> (u64, u64) {
    ddc_core::platform::meta::file_id(&fs::symlink_metadata(path).unwrap()).unwrap()
}

/// Sets the Unix permission bits of `path`.
pub fn set_mode(path: impl AsRef<Path>, mode: u32) {
    let path = path.as_ref();
    try_set_mode(path, mode).unwrap_or_else(|e| panic!("chmod {mode:o} {}: {e}", path.display()));
}

#[cfg(unix)]
fn try_set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn try_set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "Unix permission bits"))
}

/// A symlink at `link` to `target`; Unix only, as [`set_mode`].
#[cfg(unix)]
pub fn symlink(target: impl AsRef<Path>, link: impl AsRef<Path>) {
    let link = link.as_ref();
    std::os::unix::fs::symlink(target, link).unwrap_or_else(|e| panic!("symlink {}: {e}", link.display()));
}

#[cfg(not(unix))]
pub fn symlink(_target: impl AsRef<Path>, link: impl AsRef<Path>) {
    panic!("symlink {}: Unix only", link.as_ref().display());
}
