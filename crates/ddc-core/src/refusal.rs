//! The JSON error envelope (ADR "Platform seam: one platform module per OS
//! with declared capabilities", §7): what a command prints on stdout, where
//! its report would have gone, when it refuses under `--json`. Schema:
//! `docs/error.schema.json`.
//!
//! Readers branch on the exit code first, then on the `error` key. The
//! version key is `error_schema_version`, never `schema_version`, so no
//! report decoder reads the envelope's header as its own.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::disk::ActionId;
use crate::platform::{Capability, Platform, Refusal};

/// Version of [`ErrorReport`]. A new [`ErrorCode`] bumps it.
pub const ERROR_SCHEMA_VERSION: u32 = 1;

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
                simulated: u.simulated,
            },
            Refusal::CapabilityAbsent(c) => ErrorBody {
                code: ErrorCode::CapabilityAbsent,
                message: message.into(),
                platform: Some(c.platform),
                capability: Some(c.capability),
                action: c.action,
                simulated: false,
            },
        };
        Self { error_schema_version: ERROR_SCHEMA_VERSION, error: body }
    }
}
