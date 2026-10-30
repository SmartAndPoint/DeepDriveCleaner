//! The platform seam (ADR "Platform seam: one platform module per OS with
//! declared capabilities"; story "Platform seam: ADR and capability model",
//! DDC-007): one child module per OS, chosen at compile time and re-exported
//! here, so shared code never names an OS.
//!
//! This story brings the constants: which platform this build is for, what
//! it can do ([`CAPABILITIES`]) and whether it may scan or change anything
//! at all ([`SUPPORTED`]). Story "Move OS calls behind the platform module"
//! moves `sys/` here and pins each function's signature once, as
//! `const _: fn(&Path) -> io::Result<StatFs> = imp::statfs;`.
//!
//! [`SUPPORTED`] is each OS module's literal, never an environment variable,
//! a settings key or a flag: nothing a user or an agent sets can turn it on.
//! The one build knob, the feature `simulate-unported`, only turns it off.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::disk::ActionId;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
use linux as imp;
#[cfg(target_os = "macos")]
use macos as imp;
#[cfg(target_os = "windows")]
use windows as imp;

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
compile_error!("deepdrivecleaner builds for macOS, Linux and Windows only (ADR \"Platform seam\", §1)");

/// The operating system a build is for, as `std::env::consts::OS` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Platform {
    Macos,
    Linux,
    Windows,
}

impl Platform {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::Linux => "linux",
            Self::Windows => "windows",
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a platform can do beyond listing, measuring and removing files
/// (ADR "Platform seam", §3). A closed list in declaration order: a new
/// value bumps the schema version of every report that carries the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    /// The clone-aware private size where clones are expected (APFS).
    /// Without it `private_bytes` is never present.
    PrivateSize,
    /// The in-use checks before a delete (safety ADR §5). Its absence is no
    /// permission to delete without them.
    InUseCheck,
    /// Evicted cloud files are recognised and never downloaded while scanning.
    DatalessFiles,
    /// A permission system (TCC) can withhold parts of the home folder.
    /// Without it `full_disk_access` is `true`.
    FullDiskAccess,
    /// Local backup snapshots that delay freeing space are counted. Without
    /// it a clean report's `local_snapshots` is 0 and has no meaning.
    LocalSnapshots,
    /// `ddc disk` and the `disk free` actions on its rows.
    WholeDisk,
    /// `ddc disk agents`.
    CodingAgents,
    /// `ddc disk brew-packages` and the `disk free` actions on its rows.
    Homebrew,
    /// `ddc schedule`.
    Schedule,
    /// The background scan shows a notification.
    Notifications,
}

impl Capability {
    /// Every capability, in declaration order.
    pub const ALL: [Capability; 10] = [
        Self::PrivateSize,
        Self::InUseCheck,
        Self::DatalessFiles,
        Self::FullDiskAccess,
        Self::LocalSnapshots,
        Self::WholeDisk,
        Self::CodingAgents,
        Self::Homebrew,
        Self::Schedule,
        Self::Notifications,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PrivateSize => "private-size",
            Self::InUseCheck => "in-use-check",
            Self::DatalessFiles => "dataless-files",
            Self::FullDiskAccess => "full-disk-access",
            Self::LocalSnapshots => "local-snapshots",
            Self::WholeDisk => "whole-disk",
            Self::CodingAgents => "coding-agents",
            Self::Homebrew => "homebrew",
            Self::Schedule => "schedule",
            Self::Notifications => "notifications",
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The platform this build is for.
pub const PLATFORM: Platform = imp::PLATFORM;

/// What this build can do on its platform: a constant, never probed per run
/// (ADR §3). Facts about a run have their own fields.
pub const CAPABILITIES: &[Capability] = imp::CAPABILITIES;

/// Whether this build may walk a tree or change anything. False on a
/// platform not ported yet, and in a build with `simulate-unported`.
pub const SUPPORTED: bool = imp::SUPPORTED && !cfg!(feature = "simulate-unported");

/// Built with `simulate-unported`: [`SUPPORTED`] is false by that, so a
/// refusal says so instead of blaming the platform.
const SIMULATED: bool = cfg!(feature = "simulate-unported");

/// The refusal of an entry point on a platform that is not supported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unsupported {
    pub platform: Platform,
    /// The build has `simulate-unported`.
    pub simulated: bool,
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.simulated {
            f.write_str("this build was made with simulate-unported and refuses every scan and clean")
        } else {
            write!(f, "deepdrivecleaner does not support {} yet", self.platform)
        }
    }
}

impl std::error::Error for Unsupported {}

/// `Ok` when this build may walk a tree or change anything: the first
/// statement of every core entry point that does (ADR §5).
pub fn require_supported() -> Result<(), Unsupported> {
    if SUPPORTED { Ok(()) } else { Err(Unsupported { platform: PLATFORM, simulated: SIMULATED }) }
}

/// A command, or a `disk free` action, that needs a capability this
/// platform does not declare (ADR §3, §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityAbsent {
    pub platform: Platform,
    pub capability: Capability,
    /// The `disk free` action that needs it; `None` for a whole command.
    pub action: Option<ActionId>,
}

impl fmt::Display for CapabilityAbsent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.action {
            Some(action) => write!(
                f,
                "{} needs the capability {}, which deepdrivecleaner does not have on {}",
                action.as_str(),
                self.capability,
                self.platform
            ),
            None => write!(f, "deepdrivecleaner has no {} capability on {}", self.capability, self.platform),
        }
    }
}

impl std::error::Error for CapabilityAbsent {}

/// `Ok` when `declared` holds `capability`; otherwise its refusal, naming
/// `action` when a `disk free` action needs it.
pub fn require(
    capability: Capability,
    declared: &[Capability],
    action: Option<ActionId>,
) -> Result<(), CapabilityAbsent> {
    if declared.contains(&capability) {
        Ok(())
    } else {
        Err(CapabilityAbsent { platform: PLATFORM, capability, action })
    }
}

/// A refusal by the platform: the code exits with 69 (ADR §6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    Unsupported(Unsupported),
    CapabilityAbsent(CapabilityAbsent),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(u) => u.fmt(f),
            Self::CapabilityAbsent(c) => c.fmt(f),
        }
    }
}

/// The platform refusal in `err` or its sources, if any (ADR §5): an
/// [`Unsupported`] or [`CapabilityAbsent`] itself, the `Unsupported` variants
/// of the core's error enums, or an `io::Error` whose payload is one of the
/// two. The kind `io::ErrorKind::Unsupported` alone is never enough: std
/// gives it to `ENOTSUP`, and the core returns it for other reasons too.
pub fn refusal_in(err: &(dyn std::error::Error + 'static)) -> Option<Refusal> {
    let mut link = Some(err);
    while let Some(err) = link {
        if let Some(found) = refusal_at(err) {
            return Some(found);
        }
        link = err.source();
    }
    None
}

fn refusal_at(err: &(dyn std::error::Error + 'static)) -> Option<Refusal> {
    use crate::exec::ExecError;
    use crate::report::ScanError;
    use crate::scan::WalkSetupError;

    if let Some(io) = err.downcast_ref::<std::io::Error>() {
        let payload = io.get_ref()?;
        return payload
            .downcast_ref::<Unsupported>()
            .map(|u| Refusal::Unsupported(*u))
            .or_else(|| payload.downcast_ref::<CapabilityAbsent>().map(|c| Refusal::CapabilityAbsent(*c)));
    }
    let unsupported = err
        .downcast_ref::<Unsupported>()
        .or_else(|| match err.downcast_ref::<WalkSetupError>() {
            Some(WalkSetupError::Unsupported(u)) => Some(u),
            _ => None,
        })
        .or_else(|| match err.downcast_ref::<ScanError>() {
            Some(ScanError::Unsupported(u) | ScanError::Walk(WalkSetupError::Unsupported(u))) => Some(u),
            _ => None,
        })
        .or_else(|| match err.downcast_ref::<ExecError>() {
            Some(ExecError::Unsupported(u)) => Some(u),
            _ => None,
        });
    unsupported
        .map(|u| Refusal::Unsupported(*u))
        .or_else(|| err.downcast_ref::<CapabilityAbsent>().map(|c| Refusal::CapabilityAbsent(*c)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_names_are_those_of_the_json() {
        for c in Capability::ALL {
            assert_eq!(serde_json::to_value(c).unwrap(), c.as_str());
        }
        for p in [Platform::Macos, Platform::Linux, Platform::Windows] {
            assert_eq!(serde_json::to_value(p).unwrap(), p.as_str());
        }
    }

    #[test]
    fn a_capability_not_declared_is_refused_with_its_name() {
        let declared = [Capability::Homebrew];
        assert_eq!(require(Capability::Homebrew, &declared, None), Ok(()));
        let absent = require(Capability::WholeDisk, &declared, Some(ActionId::CondaClean)).unwrap_err();
        assert_eq!(absent.capability, Capability::WholeDisk);
        assert_eq!(absent.action, Some(ActionId::CondaClean));
        assert!(absent.to_string().contains("whole-disk"), "{absent}");
    }

    #[test]
    fn a_refusal_is_found_through_the_sources_and_in_an_io_error() {
        let unsupported = Unsupported { platform: Platform::Linux, simulated: false };
        let io = std::io::Error::new(std::io::ErrorKind::Unsupported, unsupported);
        assert_eq!(refusal_in(&io), Some(Refusal::Unsupported(unsupported)));
        let scan = crate::report::ScanError::Unsupported(unsupported);
        assert_eq!(refusal_in(&scan), Some(Refusal::Unsupported(unsupported)));
        let exec = crate::exec::ExecError::Journal(io);
        assert_eq!(refusal_in(&exec), Some(Refusal::Unsupported(unsupported)));
        // The kind alone is no refusal: std gives it to ENOSYS, and the core
        // returns it for other reasons too.
        let enosys = std::io::Error::from_raw_os_error(libc::ENOSYS);
        assert_eq!(enosys.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(refusal_in(&enosys), None);
        let other = std::io::Error::new(std::io::ErrorKind::Unsupported, "not a regular file");
        assert_eq!(refusal_in(&other), None);
        let absent = CapabilityAbsent { platform: Platform::Linux, capability: Capability::Schedule, action: None };
        let io = std::io::Error::new(std::io::ErrorKind::Unsupported, absent);
        assert_eq!(refusal_in(&io), Some(Refusal::CapabilityAbsent(absent)));
    }

    #[test]
    fn the_message_never_blames_the_platform_for_a_simulated_refusal() {
        let simulated = Unsupported { platform: Platform::Macos, simulated: true };
        assert!(simulated.to_string().contains("simulate-unported"));
        let real = Unsupported { platform: Platform::Linux, simulated: false };
        assert_eq!(real.to_string(), "deepdrivecleaner does not support linux yet");
    }
}
