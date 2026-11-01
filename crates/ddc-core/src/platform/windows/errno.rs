//! The error numbers shared code names, and what a directory read makes of
//! one, on Windows. `raw_os_error` there is a Win32 error code
//! (`GetLastError`), never a C `errno`: 5 is "access denied", not `EIO`.
//! Matched as raw numbers, as on macOS and Linux.

use crate::platform::types::ReadErrorKind;

/// `ERROR_FILE_NOT_FOUND`.
pub const ENOENT: i32 = 2;
/// `ERROR_ACCESS_DENIED`.
pub const EACCES: i32 = 5;
/// `ERROR_IO_DEVICE`: the code a read that failed for no reason it names
/// stands in for.
pub const EIO: i32 = 1117;

/// `ERROR_PATH_NOT_FOUND`: a folder on the way is gone.
const PATH_NOT_FOUND: i32 = 3;
/// `ERROR_DIRECTORY`: the name is not a folder (any more).
const NOT_A_DIRECTORY: i32 = 267;
/// `ERROR_CANT_RESOLVE_FILENAME`, `ERROR_CANT_ACCESS_FILE`: a reparse point
/// that could not be followed or opened.
const CANT_RESOLVE: i32 = 1921;
const CANT_ACCESS: i32 = 1920;
/// `ERROR_PRIVILEGE_NOT_HELD`.
const PRIVILEGE_NOT_HELD: i32 = 1314;

/// What a directory read makes of a Win32 error code.
pub fn read_error_kind(errno: i32) -> ReadErrorKind {
    match errno {
        EACCES | PRIVILEGE_NOT_HELD => ReadErrorKind::PermissionDenied,
        ENOENT | PATH_NOT_FOUND => ReadErrorKind::NotFound,
        NOT_A_DIRECTORY | CANT_RESOLVE | CANT_ACCESS => ReadErrorKind::Changed,
        _ => ReadErrorKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_error_is_read_by_its_raw_win32_code() {
        assert_eq!(read_error_kind(EACCES), ReadErrorKind::PermissionDenied);
        assert_eq!(read_error_kind(1314), ReadErrorKind::PermissionDenied, "ERROR_PRIVILEGE_NOT_HELD");
        assert_eq!(read_error_kind(ENOENT), ReadErrorKind::NotFound);
        assert_eq!(read_error_kind(3), ReadErrorKind::NotFound, "ERROR_PATH_NOT_FOUND");
        assert_eq!(read_error_kind(267), ReadErrorKind::Changed, "ERROR_DIRECTORY");
        assert_eq!(read_error_kind(1920), ReadErrorKind::Changed, "ERROR_CANT_ACCESS_FILE");
        assert_eq!(read_error_kind(1921), ReadErrorKind::Changed, "ERROR_CANT_RESOLVE_FILENAME");
        assert_eq!(read_error_kind(32), ReadErrorKind::Other, "a sharing violation is no read error kind");
        assert_eq!(read_error_kind(EIO), ReadErrorKind::Other);
    }

    /// The constants are what std reads them as, so a test that builds an
    /// error from one gets the kind its name says.
    #[test]
    fn the_constants_are_what_std_reads_them_as() {
        use std::io::{Error, ErrorKind};
        assert_eq!(Error::from_raw_os_error(ENOENT).kind(), ErrorKind::NotFound);
        assert_eq!(Error::from_raw_os_error(EACCES).kind(), ErrorKind::PermissionDenied);
    }
}
