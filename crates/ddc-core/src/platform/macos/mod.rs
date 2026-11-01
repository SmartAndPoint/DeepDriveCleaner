//! macOS (ADR "Platform seam: one platform module per OS with declared
//! capabilities", §2, §3, §5): thin wrappers over macOS system calls. With
//! `platform/op_stats.rs`, this is the only module where `unsafe` is
//! allowed; every wrapper exposes a safe API.
//!
//! The items §3 counts as no capability: directory listing by
//! `getattrlistbulk` (`bulk`); the verified-parent operations by the `*at`
//! calls (`fsops`); allocated size from `ATTR_FILE_ALLOCSIZE` and identity
//! from `st_dev` and `st_ino`; free space by `statfs`; the command runner
//! kills the process group (`signals`); `init_process` sets the I/O policy
//! (`iopolicy`) and the open-file limit (`rlimit`); the folders are `HOME`
//! and `~/Library`; the Trash is the Finder's.
#![allow(unsafe_code)]

pub mod bulk;
pub mod errno;
pub mod folders;
pub mod fsops;
pub mod iopolicy;
pub mod meta;
pub mod paths;
pub mod process;
pub mod procs;
pub mod rlimit;
pub mod signals;
pub mod statfs;
pub mod term;
#[cfg(test)]
pub mod testing;
pub mod trash;
pub mod usage;

use std::io;

use super::{Capability, Platform};

pub const PLATFORM: Platform = Platform::Macos;

/// The platform every release so far was made for.
pub const SUPPORTED: bool = true;

/// Every capability of §3.
pub const CAPABILITIES: &[Capability] = &[
    Capability::PrivateSize,
    Capability::InUseCheck,
    Capability::DatalessFiles,
    Capability::FullDiskAccess,
    Capability::LocalSnapshots,
    Capability::WholeDisk,
    Capability::CodingAgents,
    Capability::Homebrew,
    Capability::Schedule,
    Capability::Notifications,
];

/// `errno` of the last failed call on this thread.
fn last_errno() -> i32 {
    io::Error::last_os_error().raw_os_error().unwrap_or(0)
}
