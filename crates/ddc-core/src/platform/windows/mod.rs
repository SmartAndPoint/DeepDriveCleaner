//! Windows (ADR "Platform seam: one platform module per OS with declared
//! capabilities", §2, §3, §5): not supported yet. Story "Windows scanner and
//! rule catalog" (DDC-007) ports it module by module, then flips the literal
//! and declares its capabilities, in the same commit that enables its release
//! archives. `unsafe` is allowed here only for the Win32 calls through
//! `windows-sys`, each block with a `// SAFETY:` comment (seam ADR §2).

#![allow(unsafe_code)]

use super::{Capability, Platform};

pub mod errno;
pub mod folders;
pub mod rlimit;
pub mod term;
#[cfg(test)]
pub mod testing;

// The OS calls not ported yet: the shared stubs, of which `unported.rs`
// compiles out every module this folder has.
pub use super::unported::*;

pub const PLATFORM: Platform = Platform::Windows;

pub const SUPPORTED: bool = false;

pub const CAPABILITIES: &[Capability] = &[];
