//! Thin wrappers over macOS system calls. This is the only module where
//! `unsafe` is allowed; every wrapper exposes a safe API.
#![allow(unsafe_code)]

pub mod bulk;
pub mod fsops;
pub mod iopolicy;
pub mod procs;
pub mod rlimit;
pub mod signals;
pub mod statfs;

use std::io;

/// `errno` of the last failed call on this thread.
fn last_errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}
