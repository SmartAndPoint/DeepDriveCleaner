//! The Windows catalog: the shared rules, in the macOS order, resolved for
//! Windows. Story "Windows scanner and rule catalog" adds the rules only Windows has.

use super::shared;
use crate::platform::Platform;
use crate::platform::types::Redirect;
use crate::rules::Rule;

/// The folders in the home only Windows protects (safety ADR §6, Windows,
/// in the home): `AppData` and its three children, the credentials, keys,
/// certificates and cookies Windows keeps under them, the browsers' and
/// Thunderbird's profiles, gpg's folder, the store apps' and WSL's data,
/// Visual Studio's projects, and the user folders the shared section does
/// not name. Spelled with `/`, as the rules' paths are; their ancestors are
/// protected through `rules::Protected`, so `AppData/Roaming/Microsoft` is
/// not listed.
pub(super) const OWN_PROTECTED_IN_HOME: [&str; 25] = [
    "AppData",
    "AppData/Roaming",
    "AppData/Local",
    "AppData/LocalLow",
    "AppData/Roaming/Microsoft/Protect",
    "AppData/Roaming/Microsoft/Crypto",
    "AppData/Roaming/Microsoft/SystemCertificates",
    "AppData/Roaming/Microsoft/Credentials",
    "AppData/Local/Microsoft/Credentials",
    "AppData/Local/Microsoft/Vault",
    "AppData/Local/Microsoft/Windows/INetCookies",
    "AppData/Local/Google/Chrome/User Data",
    "AppData/Local/Microsoft/Edge/User Data",
    "AppData/Local/BraveSoftware/Brave-Browser/User Data",
    "AppData/Roaming/Mozilla/Firefox/Profiles",
    "AppData/Roaming/Thunderbird/Profiles",
    "AppData/Roaming/gnupg",
    "AppData/Local/Packages",
    "AppData/Local/wsl",
    "source",
    "source/repos",
    "OneDrive",
    "Videos",
    "Favorites",
    "Saved Games",
];

/// The shared protected folders and Windows's own.
pub(super) static PROTECTED_IN_HOME: [&str; 37] = super::concat(&shared::PROTECTED_IN_HOME, &OWN_PROTECTED_IN_HOME);

/// System folders that are never removed (safety ADR §6, Windows, system
/// folders), at their default places. The guard adds where the known-folder
/// API says they are and their drive roots (`platform::folders::system_folders`),
/// and refuses every drive root by its shape.
pub(super) static PROTECTED_SYSTEM: [&str; 6] =
    [r"C:\", r"C:\Windows", r"C:\Program Files", r"C:\Program Files (x86)", r"C:\ProgramData", r"C:\Users"];

/// The default places of the folders a variable redirects: an entry below
/// one is protected below the variable's value too (safety ADR §6, Windows,
/// system folders, planned 2026-11-08).
pub(super) static REDIRECTABLE: [(Redirect, &str); 2] =
    [(Redirect::Roaming, "AppData/Roaming"), (Redirect::Local, "AppData/Local")];

/// The shared rules, resolved for Windows: a named `static`, so the list has
/// one address (its rules' tags are found by it).
pub(super) static RULES: [Rule; shared::COUNT] = shared::all(Platform::Windows);
