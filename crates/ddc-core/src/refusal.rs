//! The JSON error envelope (ADR "Platform seam: one platform module per OS
//! with declared capabilities", §7): what a command prints on stdout, where
//! its report would have gone, when it refuses under `--json`. Schema:
//! `docs/error.schema.json`.
//!
//! Readers branch on the exit code first, then on the `error` key. The
//! version key is `error_schema_version`, never `schema_version`, so no
//! report decoder reads the envelope's header as its own.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::disk::ActionId;
use crate::platform::{Capability, Foreign, ForeignRoot, Platform, Refusal};

/// Version of [`ErrorReport`]. A new [`ErrorCode`] bumps it: 2 added
/// `root-not-owned`.
pub const ERROR_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorReport {
    pub error_schema_version: u32,
    pub error: ErrorBody,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorBody {
    pub code: ErrorCode,
    /// For people; never parsed.
    pub message: String,
    /// The platform the CLI was built for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<Platform>,
    /// The capability this platform does not declare (`capability-absent`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<Capability>,
    /// The `disk free` action that needs the absent capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ActionId>,
    /// The scan root that is not the user's (`root-not-owned`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(serialize_with = "crate::json_path::opt_path")]
    pub root: Option<PathBuf>,
    /// The SID of the account that owns `root`, when it was read
    /// (`root-not-owned`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    /// `root-not-owned` only: true when the owner of `root`, or this
    /// process's token, could not be read, so it is unknown whether `root`
    /// is the user's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_unreadable: Option<bool>,
    /// True only in a build made with `simulate-unported`: the refusal is a
    /// test build's, not the platform's.
    pub simulated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorCode {
    /// The platform is not supported yet: only `--version`, `--help` and
    /// `rules` run.
    PlatformNotSupported,
    /// The platform does not declare the capability the command, or a
    /// `disk free` action, needs.
    CapabilityAbsent,
    /// A scan root another account owns, in an elevated terminal (Windows),
    /// or one whose owner cannot be read there: refused with exit 2 before
    /// anything is scanned or changed.
    RootNotOwned,
}

impl ErrorReport {
    /// The envelope of `refusal`, with `message` for people.
    pub fn new(refusal: &Refusal, message: impl Into<String>) -> Self {
        let body = match *refusal {
            Refusal::Unsupported(u) => ErrorBody {
                code: ErrorCode::PlatformNotSupported,
                message: message.into(),
                platform: Some(u.platform),
                capability: None,
                action: None,
                root: None,
                owner: None,
                owner_unreadable: None,
                simulated: u.simulated,
            },
            Refusal::CapabilityAbsent(c) => ErrorBody {
                code: ErrorCode::CapabilityAbsent,
                message: message.into(),
                platform: Some(c.platform),
                capability: Some(c.capability),
                action: c.action,
                root: None,
                owner: None,
                owner_unreadable: None,
                simulated: false,
            },
        };
        Self { error_schema_version: ERROR_SCHEMA_VERSION, error: body }
    }

    /// The envelope of a root that is not the user's (`root-not-owned`),
    /// with `message` for people.
    pub fn root_not_owned(foreign: &ForeignRoot, message: impl Into<String>) -> Self {
        let (owner, unreadable) = match &foreign.why {
            Foreign::Owner(owner) => (Some(owner.clone()), false),
            Foreign::OwnerUnreadable(_) | Foreign::Token(_) => (None, true),
        };
        let body = ErrorBody {
            code: ErrorCode::RootNotOwned,
            message: message.into(),
            platform: Some(crate::platform::PLATFORM),
            capability: None,
            action: None,
            root: Some(foreign.path.clone()),
            owner,
            owner_unreadable: Some(unreadable),
            simulated: false,
        };
        Self { error_schema_version: ERROR_SCHEMA_VERSION, error: body }
    }
}
