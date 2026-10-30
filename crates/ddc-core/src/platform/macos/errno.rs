//! The errno numbers shared code names, and what a directory read makes of
//! an errno, on macOS. Matched as raw numbers, never through
//! `io::ErrorKind`: `EDEADLK` (an evicted cloud item, with materialization
//! off) has no kind of its own, and it is 35 on Linux.

use crate::platform::types::ReadErrorKind;

pub const ENOENT: i32 = libc::ENOENT;
pub const EACCES: i32 = libc::EACCES;
pub const EIO: i32 = libc::EIO;

/// What a directory read makes of `errno`.
pub fn read_error_kind(errno: i32) -> ReadErrorKind {
    match errno {
        libc::EACCES | libc::EPERM => ReadErrorKind::PermissionDenied,
        libc::ENOENT => ReadErrorKind::NotFound,
        libc::EDEADLK => ReadErrorKind::Dataless,
        libc::ELOOP | libc::ENOTDIR => ReadErrorKind::Changed,
        _ => ReadErrorKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dataless_item_is_edeadlk_by_its_raw_number() {
        assert_eq!(read_error_kind(11), ReadErrorKind::Dataless, "EDEADLK is 11 on macOS");
        assert_eq!(read_error_kind(libc::EPERM), ReadErrorKind::PermissionDenied);
        assert_eq!(read_error_kind(EACCES), ReadErrorKind::PermissionDenied);
        assert_eq!(read_error_kind(ENOENT), ReadErrorKind::NotFound);
        assert_eq!(read_error_kind(libc::ELOOP), ReadErrorKind::Changed);
        assert_eq!(read_error_kind(libc::ENOTDIR), ReadErrorKind::Changed);
        assert_eq!(read_error_kind(EIO), ReadErrorKind::Other);
    }

    /// std gives `ENOSYS` the kind `Unsupported`, and a bare kind is no
    /// refusal of the seam (ADR "Platform seam: one platform module per OS
    /// with declared capabilities", §5).
    #[test]
    fn enosys_is_unsupported_and_no_refusal() {
        let raw = std::io::Error::from_raw_os_error(libc::ENOSYS);
        assert_eq!(raw.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(crate::platform::refusal_in(&raw), None);
    }
}
