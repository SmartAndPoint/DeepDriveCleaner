//! The freedesktop Trash, through the `trash` crate (safety ADR §5,
//! amendment of 2026-10-31).

use std::io;
use std::path::Path;

/// Moves `path` to the Trash of its file system: the home trash
/// (`$XDG_DATA_HOME/Trash`) on the home's, `$topdir/.Trash/$uid` or
/// `$topdir/.Trash-$uid` on another.
pub fn move_to_trash(path: &Path) -> io::Result<()> {
    ::trash::delete(path).map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::platform::{folders, meta};

    /// The home trash's `files` and `info` folders, as the specification
    /// places them when `XDG_DATA_HOME` is not set (the test runs that way).
    fn home_trash() -> (PathBuf, PathBuf) {
        let trash = folders::home().expect("HOME").join(".local/share/Trash");
        (trash.join("files"), trash.join("info"))
    }

    /// A file on the home's file system moves to the home trash, with an
    /// info file that names where it was.
    #[test]
    fn a_file_in_the_home_moves_to_the_home_trash_with_its_origin() {
        let d = tempfile::tempdir_in(folders::home().expect("HOME")).unwrap();
        // Unique across runs: a home trash outlives them, and a name in it
        // already gets another one.
        let name = format!("ddc-trash-test{}", d.path().file_name().unwrap().to_string_lossy());
        let f = d.path().join(&name);
        fs::write(&f, b"x").unwrap();
        move_to_trash(&f).unwrap();
        assert!(!f.exists());
        let (files, info) = home_trash();
        assert!(files.join(&name).exists(), "{name} in {}", files.display());
        let record = fs::read_to_string(info.join(format!("{name}.trashinfo"))).unwrap();
        assert!(record.starts_with("[Trash Info]\n") && record.contains("DeletionDate="), "{record}");
        assert!(record.contains(&format!("Path={}", f.display())), "{record}");
    }

    /// On another file system with no trash folder of its own at its top,
    /// the crate moves the item to the home trash by copying it: the source
    /// is gone either way.
    #[test]
    fn a_file_on_another_file_system_leaves_it() {
        let d = tempfile::tempdir().unwrap();
        let name = format!("ddc-trash-other{}", d.path().file_name().unwrap().to_string_lossy());
        let f = d.path().join(&name);
        fs::write(&f, b"x").unwrap();
        let here = meta::file_id(&fs::metadata(d.path()).unwrap()).unwrap().0;
        let home = meta::file_id(&fs::metadata(folders::home().unwrap()).unwrap()).unwrap().0;
        move_to_trash(&f).unwrap();
        assert!(!f.exists());
        if here != home {
            assert!(home_trash().0.join(&name).exists(), "copied to the home trash");
        }
    }
}
