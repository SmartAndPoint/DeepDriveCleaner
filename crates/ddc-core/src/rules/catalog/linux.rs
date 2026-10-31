//! The Linux catalog: the shared rules, in the macOS order, resolved for
//! Linux. Story "Linux scanner and rule catalog" adds the rules only Linux has.

use super::shared;
use crate::platform::Platform;
use crate::rules::Rule;

/// The folders in the home only Linux protects (safety ADR "Cleanup safety
/// model", §6, amendment of 2026-10-31): XDG's data, state and program
/// folders, the keyrings, the Trash, the NSS certificate store, browser
/// and mail profiles, and `~/Videos`. The folders themselves, not their
/// contents: a cache rule still matches inside `~/.local/share`.
pub(super) const OWN_PROTECTED_IN_HOME: [&str; 9] = [
    ".local/share",
    ".local/state",
    ".local/bin",
    ".local/share/keyrings",
    ".local/share/Trash",
    ".pki",
    ".mozilla",
    ".thunderbird",
    "Videos",
];

/// The shared protected folders and Linux's own.
pub(super) static PROTECTED_IN_HOME: [&str; 21] = super::concat(&shared::PROTECTED_IN_HOME, &OWN_PROTECTED_IN_HOME);

/// System folders that are never removed (§6, amendment of 2026-10-31).
pub(super) static PROTECTED_SYSTEM: [&str; 7] = ["/", "/home", "/usr", "/etc", "/opt", "/var", "/boot"];

/// The shared rules, resolved for Linux: a named `static`, so the list has
/// one address (its rules' tags are found by it).
pub(super) static RULES: [Rule; shared::COUNT] = shared::all(Platform::Linux);
