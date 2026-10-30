//! What the target's file system accepts, probed before the tree is described.

use std::fs;
use std::io;
use std::path::Path;

use crate::desc::FsCaps;

/// Probes in `scratch`, an empty folder the generator owns. The probe files
/// stay there; the whole target goes when the run is done.
pub fn probe(scratch: &Path) -> io::Result<FsCaps> {
    fs::create_dir_all(scratch)?;
    fs::write(scratch.join("a"), "")?;
    fs::write(scratch.join("caf\u{e9}"), "")?;
    let bad = crate::platform::name(b"bad\xff").map(|n| scratch.join(n));
    Ok(FsCaps {
        case_insensitive: fs::symlink_metadata(scratch.join("A")).is_ok(),
        normalization_insensitive: fs::symlink_metadata(scratch.join("cafe\u{301}")).is_ok(),
        invalid_utf8: bad.is_some_and(|bad| fs::write(bad, "").is_ok()),
    })
}
