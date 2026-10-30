//! The Linux catalog: the shared rules, in the macOS order, resolved for
//! Linux. Story "Linux scanner and rule catalog" adds the rules only Linux has.

use super::shared;
use crate::platform::Platform;
use crate::rules::Rule;

/// The folders in the home only Linux protects: none yet. Story
/// "Linux scanner and rule catalog" adds them, and its system folders,
/// before it switches `SUPPORTED` on; a test refuses the switch without them.
pub(super) const OWN_PROTECTED_IN_HOME: [&str; 0] = [];

/// The shared protected folders and Linux's own.
pub(super) static PROTECTED_IN_HOME: [&str; 12] = super::concat(&shared::PROTECTED_IN_HOME, &OWN_PROTECTED_IN_HOME);

/// System folders that are never removed: none yet. While `SUPPORTED` is
/// false nothing is removed here at all.
pub(super) static PROTECTED_SYSTEM: [&str; 0] = [];

/// The shared rules, resolved for Linux: a named `static`, so the list has
/// one address (its rules' tags are found by it).
pub(super) static RULES: [Rule; shared::COUNT] = shared::all(Platform::Linux);
