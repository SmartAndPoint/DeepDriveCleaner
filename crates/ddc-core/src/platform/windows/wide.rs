//! Paths for this module's own Win32 calls: the absolute, verbatim
//! (`\\?\`), NUL-terminated UTF-16 form, which reaches past `MAX_PATH` and
//! is parsed by no Win32 rule (no trailing-dot or device-name rewriting).
//! Only calls made here get it; everything shown or compared uses the plain
//! form (`paths::canonicalize`).

use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::path::{Component, Path, Prefix};

/// `path` as `\\?\C:\…` or `\\?\UNC\server\share\…`, NUL-terminated.
///
/// Resolved lexically: a relative path joins the current folder, `.` is
/// dropped and `..` drops the name before it. Never through
/// `GetFullPathNameW` (what `std::path::absolute` calls), which would strip
/// a trailing dot or space and turn `NUL` into the device, so `foo.` would
/// open its sibling `foo`. A device path and an interior NUL are refused.
pub(super) fn verbatim(path: &Path) -> io::Result<Vec<u16>> {
    let refuse = |why: &'static str| io::Error::new(io::ErrorKind::InvalidInput, why);
    let joined = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir()?.join(path) };
    let mut components = joined.components();
    let mut out: Vec<u16> = Vec::new();
    match components.next() {
        Some(Component::Prefix(p)) => match p.kind() {
            Prefix::Verbatim(_) | Prefix::VerbatimDisk(_) | Prefix::VerbatimUNC(..) => {
                out.extend(joined.as_os_str().encode_wide());
                return finish(out, &refuse);
            }
            Prefix::DeviceNS(_) => return Err(refuse("a device path")),
            Prefix::Disk(drive) => {
                out.extend(format!(r"\\?\{}:", char::from(drive)).encode_utf16());
            }
            Prefix::UNC(server, share) => {
                out.extend(r"\\?\UNC\".encode_utf16());
                out.extend(server.encode_wide());
                out.push(u16::from(b'\\'));
                out.extend(share.encode_wide());
            }
        },
        _ => return Err(refuse("not an absolute Windows path")),
    };
    if components.next() != Some(Component::RootDir) {
        return Err(refuse("a drive-relative path"));
    }
    let mut names = Vec::new();
    for c in components {
        match c {
            Component::Normal(name) => names.push(name),
            Component::CurDir => {}
            Component::ParentDir => {
                names.pop().ok_or_else(|| refuse("`..` above the root"))?;
            }
            Component::RootDir | Component::Prefix(_) => return Err(refuse("a second root")),
        }
    }
    if names.is_empty() {
        // `\\?\C:` alone is the volume, not its root folder; a share's
        // root needs its separator too.
        out.push(u16::from(b'\\'));
    }
    for name in names {
        out.push(u16::from(b'\\'));
        out.extend(name.encode_wide());
    }
    finish(out, &refuse)
}

fn finish(mut out: Vec<u16>, refuse: &dyn Fn(&'static str) -> io::Error) -> io::Result<Vec<u16>> {
    if out.contains(&0) {
        return Err(refuse("a NUL inside the path"));
    }
    out.push(0);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(p: &str) -> String {
        let w = verbatim(Path::new(p)).unwrap();
        String::from_utf16(&w[..w.len() - 1]).unwrap()
    }

    #[test]
    fn a_path_becomes_verbatim() {
        assert_eq!(s(r"C:\Users\u\a.txt"), r"\\?\C:\Users\u\a.txt");
        assert_eq!(s("C:/Users/u/./x/../a"), r"\\?\C:\Users\u\a");
        assert_eq!(s(r"C:\"), r"\\?\C:\");
        assert_eq!(s(r"\\srv\share\dir"), r"\\?\UNC\srv\share\dir");
        assert_eq!(s(r"\\?\C:\already"), r"\\?\C:\already");
        assert_eq!(s(r"\\srv\share"), r"\\?\UNC\srv\share\");
        assert_eq!(s(r"C:\d\foo."), r"\\?\C:\d\foo.", "a trailing dot stays");
        assert_eq!(s(r"C:\d\NUL"), r"\\?\C:\d\NUL", "a device name stays a name");
    }

    #[test]
    fn a_device_path_a_nul_and_a_climb_above_the_root_are_refused() {
        assert!(verbatim(Path::new(r"\\.\PhysicalDrive0")).is_err());
        assert!(verbatim(Path::new("C:\\a\0b")).is_err());
        assert!(verbatim(Path::new(r"C:\..\..")).is_err());
        assert!(verbatim(Path::new("C:relative")).is_err());
    }
}
