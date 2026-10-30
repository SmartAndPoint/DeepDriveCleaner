//! macOS (ADR "Platform seam: one platform module per OS with declared
//! capabilities", §2, §3, §5). Until story "Move OS calls behind the platform
//! module" moves `sys/` here, this holds only the constants.
//!
//! The items §3 counts as no capability are all in `sys/` today: directory
//! listing by `getattrlistbulk`; the verified-parent operations by the `*at`
//! calls; allocated size from `ATTR_FILE_ALLOCSIZE` and identity from
//! `st_dev` and `st_ino`; free space by `statfs`; the command runner kills the
//! process group; `init_process` sets the I/O policy and the open-file limit;
//! the folders are `HOME` and `~/Library`; the Trash is the Finder's.

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
