//! The listing `ddc disk brew-packages --json` prints (schema
//! `docs/brew-packages.schema.json`).

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Service;

/// Version of [`BrewPackages`].
pub const BREW_PACKAGES_SCHEMA_VERSION: u32 = 1;

/// The formulae installed on request under the Homebrew area.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BrewPackages {
    /// [`BREW_PACKAGES_SCHEMA_VERSION`].
    pub schema_version: u32,
    pub ddc_version: String,
    /// `brew --prefix`, as brew printed it.
    #[serde(serialize_with = "crate::json_path::path")]
    pub prefix: PathBuf,
    /// `brew --cellar`: each keg is `<cellar>/<name>/<version>`.
    #[serde(serialize_with = "crate::json_path::path")]
    pub cellar: PathBuf,
    /// Whether brew's service jobs could be read.
    pub services: ServicesState,
    /// The formulae installed on request, largest first, one not measured
    /// after every measured one.
    pub formulae: Vec<Package>,
    /// What is orphaned now, by `full_name`: installed as a dependency,
    /// unpinned, and needed by no formula outside the set and no cask.
    pub orphans: Vec<String>,
    /// Formulae brew listed without a field ddc needs; while one is
    /// listed, no removal is offered.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub incomplete: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ServicesState {
    /// `brew services list --json` answered.
    Listed,
    /// brew has no `services` command without fetching it: not asked.
    NotKnown,
    /// It failed, or printed what ddc cannot read: no removal is offered.
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Package {
    /// The Cellar rack's name.
    pub name: String,
    /// What a removal names: `user/tap/foo` for a tap formula.
    pub full_name: String,
    pub pinned: bool,
    /// One per `installed[]` entry, in brew's order.
    pub kegs: Vec<Keg>,
    /// The kegs together; absent when one was not measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    pub service: Service,
    /// The installed formulae that need it, by `full_name`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needed_by: Vec<String>,
    /// The installed casks whose `depends_on formula:` names it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needed_by_casks: Vec<String>,
    /// What its kegs need, recursively, among the installed formulae,
    /// largest first.
    pub dependencies: Vec<Dependency>,
    /// What its removal would leave needed by nothing, by `full_name`.
    pub would_orphan: Vec<String>,
    /// Their kegs together: what the dependencies it alone holds take.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub would_orphan_bytes: Option<u64>,
    /// Pinned dependencies that would be orphans: kept.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept_pinned: Vec<String>,
    /// Why its removal is not offered; absent when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_offered: Option<NotOffered>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Keg {
    /// `installed[].version`, as brew printed it.
    pub version: String,
    /// `<cellar>/<name>/<version>`.
    #[serde(serialize_with = "crate::json_path::path")]
    pub path: PathBuf,
    /// Allocated bytes, each inode once; absent when a skip of
    /// `settings.json` lies at, above or under it, or it could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Dependency {
    pub full_name: String,
    /// Its kegs together.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Installed on request: it stays.
    pub on_request: bool,
    pub pinned: bool,
    /// Only this formula holds it: its removal orphans it.
    pub alone: bool,
}

/// Why a removal is not offered, worded by the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum NotOffered {
    /// An installed formula or cask needs it.
    Needed,
    Pinned,
    /// A service job is registered for it ([`Package::service`] says which).
    Service,
    /// brew's services could not be read.
    ServicesUnreadable,
    /// A formula brew listed lacks a field ddc needs.
    FieldMissing,
}
