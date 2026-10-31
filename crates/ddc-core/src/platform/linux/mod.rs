//! Linux (ADR "Platform seam: one platform module per OS with declared
//! capabilities", §2, §3, §5): not supported yet. Story "Linux scanner and
//! rule catalog" ports the OS calls one module at a time, then flips the
//! literal and declares its capabilities, in the same commit that enables
//! its release archives. `unsafe` is allowed here only for the calls
//! `rustix` does not wrap (seam ADR §2, amended 2026-10-31): `wait4` with the
//! child's resource usage in `usage`, the handlers in `signals`.

#![allow(unsafe_code)]

use super::{Capability, Platform};

pub mod bulk;
pub mod errno;
pub mod folders;
pub mod fsops;
pub mod meta;
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

// The OS calls not ported yet: the shared stubs, of which `unported.rs`
// compiles out every module this folder has.
pub use super::unported::*;

pub const PLATFORM: Platform = Platform::Linux;

pub const SUPPORTED: bool = false;

pub const CAPABILITIES: &[Capability] = &[];
