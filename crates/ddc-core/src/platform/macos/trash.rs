//! The macOS Trash.

use std::ffi::OsStr;
use std::io;
use std::path::Path;

use super::fsops::DirFd;
use crate::platform::types::{TrashError, TrashPlace};

/// Moves the item at `path` to the Trash through `NSFileManager`: no
/// AppleScript, so no Automation prompt. Every volume has a Trash here, so
/// the place is not told.
pub fn trash_item(
    _parent: &DirFd,
    _name: &OsStr,
    path: &Path,
    _id: (u64, u64),
) -> Result<Option<TrashPlace>, TrashError> {
    move_to_trash(path)?;
    Ok(None)
}

/// The dry run's answer: every volume has a Trash.
pub fn would_trash(_parent: &DirFd, _name: &OsStr, _path: &Path) -> Result<(), TrashError> {
    Ok(())
}

fn move_to_trash(path: &Path) -> io::Result<()> {
    use ::trash::macos::{DeleteMethod, TrashContextExtMacos as _};
    let mut ctx = ::trash::TrashContext::default();
    ctx.set_delete_method(DeleteMethod::NsFileManager);
    ctx.delete(path).map_err(io::Error::other)
}
