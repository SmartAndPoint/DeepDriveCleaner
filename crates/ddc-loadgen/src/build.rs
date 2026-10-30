//! Creates a [`Desc`] on disk through the platform's builder
//! (`platform::build`: the `*at` calls on macOS and Linux, so a path longer
//! than `PATH_MAX` can be built), with the contents every platform writes.

use std::io;
use std::path::Path;

use crate::desc::{BLOCK, Desc};

/// Builds `desc` inside the empty directory `tree`.
pub fn build(desc: &Desc, tree: &Path) -> io::Result<()> {
    crate::platform::build(desc, tree)
}

/// A text with `{tree}` resolved, padded with newlines to exactly one block:
/// every parser of a marker the rules read skips blank lines or trims.
pub fn padded(text: &str, tree: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = text.as_bytes().to_vec();
    if let Some(i) = text.find("{tree}") {
        bytes.splice(i..i + "{tree}".len(), tree.as_os_str().as_encoded_bytes().iter().copied());
    }
    if bytes.len() > BLOCK as usize {
        return Err(io::Error::other(format!("a marker text is longer than {BLOCK} bytes")));
    }
    if !bytes.is_empty() {
        bytes.resize(BLOCK as usize, b'\n');
    }
    Ok(bytes)
}

/// One block of non-zero bytes, so no file system stores it as a hole.
pub fn content() -> Vec<u8> {
    (0..BLOCK).map(|i| (i % 251) as u8 + 1).collect()
}
