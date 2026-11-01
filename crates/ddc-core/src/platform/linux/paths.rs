//! Canonical paths and the key paths are compared by (story "Windows scanner
//! and rule catalog", DDC-007, step 2). Here a canonical path is the
//! kernel's `realpath`, and a path is its own key: the catalog compares
//! component by component, as before.

use std::borrow::Cow;
use std::io;
use std::path::{Path, PathBuf};

/// `realpath(3)`: absolute, every symlink resolved.
#[allow(clippy::disallowed_methods)] // the one call of std's canonicalize; everything else goes through here
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    std::fs::canonicalize(path)
}

/// The path itself.
pub fn key(path: &Path) -> Cow<'_, Path> {
    Cow::Borrowed(path)
}
