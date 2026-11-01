//! Canonical paths and the key paths are compared by, on Windows (story
//! "Windows scanner and rule catalog", DDC-007, step 2).
//!
//! std's canonicalize opens the path and asks for its final name
//! (`GetFinalPathNameByHandleW`), which resolves links, 8.3 short names and
//! the case as stored, but returns the verbatim form `\\?\C:\…`. Reports and
//! comparisons use the plain form `C:\…`; std adds the verbatim prefix again
//! by itself where a call needs it (a path over `MAX_PATH`). A path whose
//! names the plain form cannot spell (a trailing dot or space, a reserved
//! device name) keeps the prefix: it then never equals a scanned path and
//! fails closed at the identity check.
//!
//! The key folds what Windows treats as the same name: both separators, and
//! the case of each UTF-16 unit (NTFS compares names case-insensitively by
//! default). Catalog paths are written with `/`, the disk gives `\`.

use std::borrow::Cow;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Component, Path, PathBuf, Prefix};

/// The final path of `path`, without the verbatim prefix where the plain
/// form spells the same names.
#[allow(clippy::disallowed_methods)] // the one call of std's canonicalize; everything else goes through here
pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
    std::fs::canonicalize(path).map(simplified)
}

/// `path` with `\\?\C:\` turned into `C:\` and `\\?\UNC\server\share` into
/// `\\server\share`, when every name can be spelled plainly.
fn simplified(path: PathBuf) -> PathBuf {
    let mut components = path.components();
    let plain = match components.next() {
        Some(Component::Prefix(p)) => match p.kind() {
            Prefix::VerbatimDisk(drive) => {
                let mut s = OsString::from(char::from(drive).to_string());
                s.push(":");
                s
            }
            Prefix::VerbatimUNC(server, share) => {
                let mut s = OsString::from(r"\\");
                s.push(server);
                s.push(r"\");
                s.push(share);
                s
            }
            _ => return path,
        },
        _ => return path,
    };
    let rest: Vec<Component<'_>> = components.collect();
    let spellable = rest.iter().all(|c| match c {
        Component::RootDir => true,
        Component::Normal(name) => plainly_spelled(name),
        _ => false,
    });
    if !spellable {
        return path;
    }
    let mut out = PathBuf::from(plain);
    out.push(r"\");
    for c in rest.into_iter().filter(|c| matches!(c, Component::Normal(_))) {
        out.push(c.as_os_str());
    }
    out
}

/// A name the plain (Win32) form keeps as it is: no trailing dot or space,
/// which Win32 strips, and no device name such as `NUL` or `com1.txt`.
fn plainly_spelled(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else { return true };
    if name.ends_with('.') || name.ends_with(' ') {
        return false;
    }
    let stem = name.split('.').next().unwrap_or(name).trim_end().to_ascii_uppercase();
    let device = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0');
    !device
}

/// `path` with `\` as its only separator and each UTF-16 unit upper-cased
/// where it has a single upper-case form in the Basic Multilingual Plane.
pub fn key(path: &Path) -> Cow<'_, Path> {
    let mut out = PathBuf::new();
    for c in path.components() {
        out.push(upper(c.as_os_str()));
    }
    Cow::Owned(out)
}

fn upper(name: &OsStr) -> OsString {
    let wide: Vec<u16> = name
        .encode_wide()
        .map(|u| {
            let Some(c) = char::from_u32(u32::from(u)) else { return u };
            let mut up = c.to_uppercase();
            match (up.next(), up.next()) {
                (Some(one), None) => u16::try_from(u32::from(one)).unwrap_or(u),
                _ => u,
            }
        })
        .collect();
    OsString::from_wide(&wide)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_verbatim_prefix_goes_where_the_names_are_spellable() {
        let s = |p: &str| simplified(PathBuf::from(p));
        assert_eq!(s(r"\\?\C:\Users\u\AppData"), PathBuf::from(r"C:\Users\u\AppData"));
        assert_eq!(s(r"\\?\C:\"), PathBuf::from(r"C:\"));
        assert_eq!(s(r"\\?\UNC\srv\share\dir"), PathBuf::from(r"\\srv\share\dir"));
        assert_eq!(s(r"C:\already\plain"), PathBuf::from(r"C:\already\plain"));
        assert_eq!(s(r"\\?\C:\dir\name."), PathBuf::from(r"\\?\C:\dir\name."), "a trailing dot");
        assert_eq!(s(r"\\?\C:\dir\name "), PathBuf::from(r"\\?\C:\dir\name "), "a trailing space");
        assert_eq!(s(r"\\?\C:\dir\nul.txt"), PathBuf::from(r"\\?\C:\dir\nul.txt"), "a device name");
        assert_eq!(s(r"\\?\C:\dir\COM1"), PathBuf::from(r"\\?\C:\dir\COM1"));
        assert_eq!(s(r"\\?\C:\dir\COM0"), PathBuf::from(r"C:\dir\COM0"), "COM0 is no device");
        assert_eq!(s(r"\\?\C:\dir\console"), PathBuf::from(r"C:\dir\console"));
    }

    #[test]
    fn a_key_folds_case_and_separators() {
        assert_eq!(
            key(Path::new("C:/Users/u/AppData/Local/Microsoft/Windows/INetCookies")),
            key(Path::new(r"c:\users\U\appdata\local\microsoft\windows\InetCookies"))
        );
        assert_eq!(key(Path::new(r"C:\Пользователи\Ä")), key(Path::new(r"c:\пользователи\ä")));
        assert_ne!(key(Path::new(r"C:\a\b")), key(Path::new(r"C:\a\bc")));
        // A unit with no single upper-case form stays as it is.
        assert_eq!(key(Path::new("C:\\ß")), Cow::<Path>::Owned(PathBuf::from("C:\\ß")));
    }

    /// A real folder: the final path drops the prefix, resolves the case
    /// and resolves an 8.3 name where the volume makes them.
    #[test]
    fn a_real_folder_comes_back_plain_in_its_stored_case() {
        let d = tempfile::tempdir().unwrap();
        let named = d.path().join("Mixed Case Folder");
        std::fs::create_dir(&named).unwrap();
        let canonical = canonicalize(&d.path().join("MIXED CASE FOLDER")).unwrap();
        assert!(!canonical.as_os_str().to_string_lossy().starts_with(r"\\?\"), "{}", canonical.display());
        assert_eq!(canonical.file_name().unwrap(), "Mixed Case Folder");
    }
}
