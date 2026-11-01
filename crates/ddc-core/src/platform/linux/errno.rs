//! The errno numbers shared code names, and what a directory read makes of
//! an errno, on Linux. Matched as raw numbers, as on macOS. Linux has no
//! dataless files: its `EDEADLK` (35) is a lock error, never `Dataless`.

use rustix::io::Errno;

use crate::platform::types::ReadErrorKind;

pub const ENOENT: i32 = Errno::NOENT.raw_os_error();
pub const EACCES: i32 = Errno::ACCESS.raw_os_error();
pub const EIO: i32 = Errno::IO.raw_os_error();

/// What a directory read makes of `errno`.
pub fn read_error_kind(errno: i32) -> ReadErrorKind {
    match Errno::from_raw_os_error(errno) {
        Errno::ACCESS | Errno::PERM => ReadErrorKind::PermissionDenied,
        Errno::NOENT => ReadErrorKind::NotFound,
        Errno::LOOP | Errno::NOTDIR => ReadErrorKind::Changed,
        // `openat2` with `RESOLVE_NO_XDEV`: a mount on the way (safety ADR,
        // amendment of 2026-11-01).
        Errno::XDEV => ReadErrorKind::OtherFilesystem,
        _ => ReadErrorKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_error_is_read_by_its_raw_linux_number() {
        assert_eq!(read_error_kind(EACCES), ReadErrorKind::PermissionDenied);
        assert_eq!(read_error_kind(1), ReadErrorKind::PermissionDenied, "EPERM");
        assert_eq!(read_error_kind(ENOENT), ReadErrorKind::NotFound);
        assert_eq!(read_error_kind(40), ReadErrorKind::Changed, "ELOOP");
        assert_eq!(read_error_kind(20), ReadErrorKind::Changed, "ENOTDIR");
        assert_eq!(read_error_kind(35), ReadErrorKind::Other, "EDEADLK is no dataless item on Linux");
        assert_eq!(read_error_kind(EIO), ReadErrorKind::Other);
        assert_eq!(read_error_kind(18), ReadErrorKind::OtherFilesystem, "EXDEV, a mount crossed");
    }
}
