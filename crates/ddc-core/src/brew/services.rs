//! `brew services list --json`, read leniently, and whether a formula has a
//! service job its removal would leave behind: `brew uninstall` does not
//! stop one.
//!
//! - A `status` other than `none` (`started`, `scheduled`, `stopped`,
//!   `error`, `unknown`, `other`, or one not yet known) leaves a launchd job
//!   pointing at a removed binary.
//! - A `user` of `root`: brew, run as the user, asks only the user's launchd
//!   domain, so it cannot say whether a root job is loaded.
//! - `status: none` with a `file` that exists under `~/Library/LaunchAgents`:
//!   a plist left registered but not loaded. The keg's own
//!   `homebrew.mxcl.<name>.plist` of a service never started is not one.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One entry, each field read as any string or absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    /// The formula's short name.
    pub name: Option<String>,
    pub status: Option<String>,
    pub user: Option<String>,
    pub file: Option<PathBuf>,
}

/// What brew said about its services.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Services {
    Listed(Vec<Entry>),
    /// brew has no `services` command without fetching it: not asked.
    NotKnown,
    /// brew has it, and it failed or printed what ddc cannot read.
    Unreadable,
}

/// A formula's service, as the listing and the re-read judge it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Service {
    /// No service of the user's registered with launchd.
    None,
    /// brew could not be asked: the removal is offered with a note.
    NotKnown,
    /// `brew services list` failed: nothing is offered.
    Unreadable,
    /// A job with a `status` other than `none`.
    Running,
    /// A root service is registered for it; its state is not known.
    Root,
    /// The user's plist is registered but not running.
    Registered,
}

impl Service {
    /// The removal may go ahead.
    pub fn allows_removal(self) -> bool {
        matches!(self, Service::None | Service::NotKnown)
    }
}

/// `None` when the output is not a JSON list.
pub fn parse(stdout: &str) -> Option<Vec<Entry>> {
    let doc: Value = serde_json::from_str(stdout.trim()).ok()?;
    let string = |v: &Value, key: &str| v.get(key).and_then(Value::as_str).map(str::to_owned);
    Some(
        doc.as_array()?
            .iter()
            .map(|v| Entry {
                name: string(v, "name"),
                status: string(v, "status"),
                user: string(v, "user"),
                file: string(v, "file").map(PathBuf::from),
            })
            .collect(),
    )
}

impl Services {
    /// The service of the formula whose short name is `name`; `home` is the
    /// user's, whose `Library/LaunchAgents` holds a registered plist.
    pub fn of(&self, name: &str, home: &Path) -> Service {
        let entries = match self {
            Services::Listed(entries) => entries,
            Services::NotKnown => return Service::NotKnown,
            Services::Unreadable => return Service::Unreadable,
        };
        let agents = home.join("Library/LaunchAgents");
        let mut verdict = Service::None;
        for e in entries.iter().filter(|e| e.name.as_deref() == Some(name)) {
            let this = if e.user.as_deref() == Some("root") {
                Service::Root
            } else if e.status.as_deref() != Some("none") {
                Service::Running
            } else if e.file.as_deref().is_some_and(|f| f.starts_with(&agents) && f.symlink_metadata().is_ok()) {
                Service::Registered
            } else {
                Service::None
            };
            verdict = worse(verdict, this);
        }
        verdict
    }
}

/// The verdict to keep of two entries for one formula.
fn worse(a: Service, b: Service) -> Service {
    let rank = |s: Service| match s {
        Service::Root => 3,
        Service::Running => 2,
        Service::Registered => 1,
        _ => 0,
    };
    if rank(b) > rank(a) { b } else { a }
}
