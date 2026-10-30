//! The macOS Trash.

use std::io;
use std::path::Path;

/// Moves `path` to the Trash through `NSFileManager`: no AppleScript, so no
/// Automation prompt.
pub fn move_to_trash(path: &Path) -> io::Result<()> {
    use ::trash::macos::{DeleteMethod, TrashContextExtMacos as _};
    let mut ctx = ::trash::TrashContext::default();
    ctx.set_delete_method(DeleteMethod::NsFileManager);
    ctx.delete(path).map_err(io::Error::other)
}
