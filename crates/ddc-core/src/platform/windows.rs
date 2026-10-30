//! Windows (ADR "Platform seam: one platform module per OS with declared
//! capabilities", §2, §3, §5): not supported yet. Story "Windows scanner and
//! rule catalog" flips the literal and declares its capabilities, in the
//! same commit that enables its release archives.

use super::{Capability, Platform};

pub const PLATFORM: Platform = Platform::Windows;

pub const SUPPORTED: bool = false;

pub const CAPABILITIES: &[Capability] = &[];
