//! What Docker Desktop keeps in its disk image, read only (story "Docker
//! Desktop: what is inside its disk image, and what a clean frees"; menu bar
//! ADR §6 item 10). It runs the clean's own readiness probe, then one
//! `docker system df -v`, and nothing else: no scan, dry run or re-check
//! comes here, and no command here removes anything. Its counts and sizes
//! name no image, container or volume; a failure carries docker's own last
//! line.

mod parse;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::exec::{CommandRunner, Readiness, readiness, said};
use crate::rules::DOCKER_READY;

pub use parse::{SizeError, size};

/// Version of [`DockerContents`] (`docs/docker.schema.json`).
pub const DOCKER_SCHEMA_VERSION: u32 = 1;

/// The one read: every image, container, volume and build-cache record of
/// Docker Desktop's engine, as one JSON object.
pub const SYSTEM_DF: [&str; 7] = ["--context", "desktop-linux", "system", "df", "-v", "--format", "{{json .}}"];

/// `system df` walks every image, container and volume; past this it has
/// failed.
pub const DF_TIMEOUT: Duration = Duration::from_secs(60);

/// What `ddc disk docker --json` prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DockerContents {
    /// [`DOCKER_SCHEMA_VERSION`].
    pub schema_version: u32,
    pub ddc_version: String,
    pub result: DockerResult,
}

/// What docker answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "kebab-case")]
pub enum DockerResult {
    /// `system df -v` answered and every figure was read.
    Listed {
        /// Every image; its bytes are the sum of each image's unique size,
        /// shared layers left out.
        images: Group,
        /// Every container; its bytes are the writable layers.
        containers: Group,
        /// Every volume; its bytes are the sizes docker gave (a volume
        /// without one is counted, not sized, as `docker system df` does).
        volumes: Group,
        /// Every build-cache record; its bytes leave out what is shared with
        /// images, as `docker system df` does.
        build_cache: Group,
        /// What the clean's two prune commands would free, about.
        estimate: Estimate,
    },
    /// No `docker` command was found.
    NotInstalled,
    /// The readiness probe failed; `detail` is its own words, as in a
    /// clean's `tool-not-ready` skip.
    NotReady { detail: String },
    /// `system df -v` failed; `detail` is the last line it printed, in the
    /// probe's words when it is one the probe knows.
    Failed { detail: String },
    /// `system df -v` did not finish within this.
    TimedOut { seconds: u64 },
    /// `system df -v` printed what ddc cannot read.
    Unreadable { detail: String },
}

/// A kind of record: how many, and their size together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Group {
    pub count: u64,
    /// Absent when one of them has no size (`N/A`); volumes without one
    /// are left out of it instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

/// What `docker builder prune -f` and `docker image prune -f` would free.
/// Both parts come from sizes docker rounds, so it is "about".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub struct Estimate {
    /// Build-cache records not in use, not shared with an image, neither
    /// `internal` nor `frontend`.
    pub build_cache: u64,
    /// Images with repository and tag `<none>` and no container, by their
    /// unique size; absent when one of them has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dangling_images: Option<u64>,
    /// The two together; absent while a part is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
}

/// The probe, a cancel check, then `system df -v`; `None` when a cancel
/// arrived, and then no further command runs.
pub fn contents(commands: &dyn CommandRunner, cancel: &AtomicBool) -> Option<DockerContents> {
    let result = match readiness(&DOCKER_READY, commands, cancel) {
        Readiness::Cancelled => return None,
        Readiness::Unavailable => DockerResult::NotInstalled,
        Readiness::NotReady(detail) => DockerResult::NotReady { detail },
        Readiness::Ready => {
            let Some(exe) = commands.find(DOCKER_READY.program) else {
                return Some(wrap(DockerResult::NotInstalled));
            };
            let args: Vec<String> = SYSTEM_DF.iter().map(|a| (*a).to_owned()).collect();
            if cancel.load(Ordering::SeqCst) {
                return None;
            }
            let ran = commands.run(&exe, &args, &[], DF_TIMEOUT);
            if cancel.load(Ordering::SeqCst) {
                return None;
            }
            match ran {
                Ok(out) if out.success => parse::listed(&out.stdout),
                // docker 29's "failed to connect" names the socket's path:
                // a known line is said in the probe's words.
                Ok(out) => DockerResult::Failed {
                    detail: said(&DOCKER_READY, &crate::exec::tail(&out.stdout, &out.stderr))
                        .unwrap_or_else(|| "docker system df exited with an error".to_owned()),
                },
                Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                    DockerResult::TimedOut { seconds: DF_TIMEOUT.as_secs() }
                }
                Err(e) => DockerResult::Failed { detail: e.to_string() },
            }
        }
    };
    Some(wrap(result))
}

fn wrap(result: DockerResult) -> DockerContents {
    DockerContents { schema_version: DOCKER_SCHEMA_VERSION, ddc_version: env!("CARGO_PKG_VERSION").to_owned(), result }
}
