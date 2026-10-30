//! The load generator on Windows: no builder yet. [`builds`] says so before
//! a target is prepared, so a test that reaches the builder through
//! `generate` skips (`ddc_loadgen::skip_unported`) rather than fail; the
//! Windows story gives [`build`] a body.

use std::ffi::OsString;
use std::fs::Metadata;
use std::io;
use std::path::Path;
use std::process::ExitStatus;

use crate::desc::Desc;

/// No image tier here (DDC-007), so a marked folder takes any size.
pub const TEMP_MAX: Option<u64> = None;

fn unsupported<T>() -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::Unsupported, "ddc-loadgen cannot build a tree on Windows yet"))
}

pub fn builds() -> io::Result<()> {
    unsupported()
}

pub fn build(_desc: &Desc, _tree: &Path) -> io::Result<()> {
    unsupported()
}

/// Only UTF-8: a Windows name is UTF-16.
pub fn name(bytes: &[u8]) -> Option<OsString> {
    std::str::from_utf8(bytes).ok().map(OsString::from)
}

/// Windows has no signals: a process ends with an exit code.
pub fn exit_signal(_status: &ExitStatus) -> Option<i32> {
    None
}

/// Fails closed: no folder counts as this user's, so none is taken as a target.
pub fn is_mine(_md: &Metadata) -> bool {
    false
}

pub fn owner_has_rwx(_md: &Metadata) -> bool {
    true
}

pub fn unlock_dir(_dir: &Path) -> io::Result<()> {
    Ok(())
}
