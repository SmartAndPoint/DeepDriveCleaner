//! The macOS Trash.

use std::ffi::OsStr;
use std::io;
use std::path::Path;

use super::fsops::DirFd;
use crate::platform::types::{TrashAsk, TrashDone, TrashError};

/// Moves the item at `path` to the Trash through `NSFileManager`: no
/// AppleScript, so no Automation prompt. Every volume has a Trash here, so
/// the place is not told.
pub fn trash_item(_parent: &DirFd, _name: &OsStr, path: &Path, _id: (u64, u64)) -> Result<TrashDone, TrashError> {
    move_to_trash(path)?;
    Ok(TrashDone::Moved(None))
}

/// What one clean or one dry run keeps between its asks: nothing, since
/// every volume has a Trash and it never refuses for room.
#[derive(Debug, Default)]
pub struct Budget;

/// The check before a move, and the dry run's answer: every volume has a
/// Trash.
pub fn admit(_budget: &mut Budget, _parent: &DirFd, _asks: &[TrashAsk<'_>]) -> Result<(), TrashError> {
    Ok(())
}

fn move_to_trash(path: &Path) -> io::Result<()> {
    use ::trash::macos::{DeleteMethod, TrashContextExtMacos as _};
    let mut ctx = ::trash::TrashContext::default();
    ctx.set_delete_method(DeleteMethod::NsFileManager);
    ctx.delete(path).map_err(io::Error::other)
}
