//! Linux (ADR "Platform seam: one platform module per OS with declared
//! capabilities", §2, §3, §5): not supported yet. Story "Linux scanner and
//! rule catalog" ports the OS calls one module at a time, then flips the
//! literal and declares its capabilities, in the same commit that enables
//! its release archives.

use super::{Capability, Platform};

pub mod errno;
pub mod folders;
pub mod meta;
pub mod process;
pub mod rlimit;
pub mod term;
#[cfg(test)]
pub mod testing;

// The OS calls not ported yet: the shared stubs, of which `unported.rs`
// compiles out every module this folder has.
pub use super::unported::*;

pub const PLATFORM: Platform = Platform::Linux;

pub const SUPPORTED: bool = false;

pub const CAPABILITIES: &[Capability] = &[];
