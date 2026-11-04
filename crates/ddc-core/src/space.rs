//! `ddc space`: whether in-use space on the home folder's volume grew since
//! the saved report, without a walk (ADR "Menu bar app: Swift shell over the
//! ddc CLI", §6 item 13; story "Popover: say when used space grew since the
//! last scan"). One `statfs`, the head of `last-scan.json` and a sample in
//! `space.json`: it takes no lock and writes nothing else. The rule lives
//! here, so the app compares nothing itself. Schema: `docs/space.schema.json`.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::env::Env;
use crate::report::volumes::StatFs;
use crate::store::{self, LAST_SCAN, SPACE};

/// Version of [`SpaceReport`]; the scan report has its own.
pub const SPACE_SCHEMA_VERSION: u32 = 1;
/// Version of `space.json`, the samples.
pub const SAMPLES_SCHEMA_VERSION: u32 = 1;
/// In-use growth that counts on any volume: 20 GiB.
pub const GREW_MIN_BYTES: u64 = 20 << 30;
/// How long `space.json` keeps a sample: 14 days.
pub const SAMPLES_KEPT: Duration = Duration::from_secs(14 * 86_400);
const HOUR_SECS: u64 = 3_600;

/// What `ddc space --json` prints. The optional fields are always written,
/// as null when absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SpaceReport {
    pub schema_version: u32,
    /// RFC 3339, UTC: when the free space was read.
    pub checked_at: String,
    /// The home folder's volume, read now.
    pub volume: SpaceVolume,
    /// What the saved report recorded for that volume. Null with no saved
    /// report, or with none that lists this volume.
    pub baseline: Option<Baseline>,
    /// In-use bytes now minus in-use bytes at the baseline, in-use being
    /// total minus free on each side: positive when in-use space grew. Null
    /// exactly when `baseline` is.
    pub in_use_delta: Option<i64>,
    /// In-use space grew by at least max(20 GiB, 5 % of `total_bytes`) since
    /// the baseline, also when `low-disk` wins; false with no baseline.
    pub grew: bool,
    pub state: SpaceState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SpaceVolume {
    #[serde(serialize_with = "crate::json_path::path")]
    pub mount_point: PathBuf,
    pub total_bytes: u64,
    /// Bytes available to an unprivileged user, as a scan reads them.
    pub free_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Baseline {
    pub source: BaselineSource,
    /// RFC 3339, copied from the saved report as it is written there: the
    /// volume's `read_at` for `clean`, the report's `generated_at` for `scan`.
    pub at: String,
    /// The volume's `free_bytes` in the saved report.
    pub free_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum BaselineSource {
    /// The scan read the free space, at the report's `generated_at`.
    Scan,
    /// A clean's edit (`ddc clean`, `ddc clean --items`) read it again, at
    /// the volume's `read_at`.
    Clean,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SpaceState {
    /// Neither of the others.
    Quiet,
    /// In-use space grew by at least the threshold since the baseline.
    Grew,
    /// Under 10 % free, unless `notify_low_disk` is false; it wins over
    /// `grew`.
    LowDisk,
}

/// The saved report's entry for the volume: the baseline, and the volume's
/// total then, for its in-use bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub baseline: Baseline,
    pub total_bytes: u64,
}

/// One reading in `space.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sample {
    /// RFC 3339, UTC.
    pub at: String,
    #[serde(serialize_with = "crate::json_path::path")]
    pub mount_point: PathBuf,
    pub total: u64,
    pub free: u64,
}

#[derive(Serialize, Deserialize)]
struct Samples {
    schema_version: u32,
    samples: Vec<Sample>,
}

/// The growth that counts on a volume of `total` bytes: max(20 GiB, 5 %).
pub fn grew_threshold(total: u64) -> u64 {
    GREW_MIN_BYTES.max(total / 20)
}

/// The rule (ADR §6 item 13): `grew` when in-use space grew by at least
/// [`grew_threshold`] of the current total; `low-disk` under 10 % free when
/// `low_disk` (`notify_low_disk` is not false), winning over `grew`;
/// otherwise `quiet`. Shrinking never fires. Without `saved` there is no
/// growth: `low-disk` or `quiet`.
pub fn decide(checked_at: String, volume: SpaceVolume, saved: Option<Saved>, low_disk: bool) -> SpaceReport {
    let in_use = |total: u64, free: u64| i128::from(total) - i128::from(free);
    let delta = saved
        .as_ref()
        .map(|s| in_use(volume.total_bytes, volume.free_bytes) - in_use(s.total_bytes, s.baseline.free_bytes));
    let grew = delta.is_some_and(|d| d >= i128::from(grew_threshold(volume.total_bytes)));
    let low = low_disk && volume.total_bytes > 0 && volume.free_bytes.saturating_mul(10) < volume.total_bytes;
    let state = if low {
        SpaceState::LowDisk
    } else if grew {
        SpaceState::Grew
    } else {
        SpaceState::Quiet
    };
    SpaceReport {
        schema_version: SPACE_SCHEMA_VERSION,
        checked_at,
        volume,
        baseline: saved.map(|s| s.baseline),
        in_use_delta: delta.map(|d| i64::try_from(d).unwrap_or(if d < 0 { i64::MIN } else { i64::MAX })),
        grew,
        state,
    }
}

/// Reads the home folder's volume and decides against the saved report.
/// Only a `statfs` that fails is an error: a saved report that is missing,
/// unreadable or without the volume gives no baseline. It writes nothing:
/// [`record`] keeps the sample.
pub fn check(env: &Env, statfs: &dyn StatFs, low_disk: bool) -> io::Result<SpaceReport> {
    let st = statfs.statfs(env.home())?;
    let volume =
        SpaceVolume { mount_point: st.mount_point, total_bytes: st.total_bytes, free_bytes: st.available_bytes };
    let saved = std::fs::read(env.data_dir().join(LAST_SCAN)).ok().and_then(|bytes| saved(&bytes, &volume.mount_point));
    Ok(decide(rfc3339(env.now()), volume, saved, low_disk))
}

/// The entry for `mount_point` in the head of `last-scan.json`: its
/// `generated_at` and `volumes`, of any schema version that has them, a
/// cancelled report's too. The findings are not read into anything.
fn saved(bytes: &[u8], mount_point: &Path) -> Option<Saved> {
    #[derive(Deserialize)]
    struct Head {
        generated_at: String,
        volumes: Vec<HeadVolume>,
    }
    #[derive(Deserialize)]
    struct HeadVolume {
        mount_point: PathBuf,
        free_bytes: u64,
        total_bytes: u64,
        #[serde(default)]
        read_at: Option<String>,
    }
    let head: Head = serde_json::from_slice(bytes).ok()?;
    let v = head.volumes.into_iter().find(|v| v.mount_point == mount_point)?;
    let (source, at) = match v.read_at {
        Some(at) => (BaselineSource::Clean, at),
        None => (BaselineSource::Scan, head.generated_at),
    };
    Some(Saved { baseline: Baseline { source, at, free_bytes: v.free_bytes }, total_bytes: v.total_bytes })
}

/// The samples of `space.json`; none when it is missing, corrupt or of
/// another version.
pub fn read_samples(env: &Env) -> Vec<Sample> {
    std::fs::read(env.data_dir().join(SPACE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Samples>(&bytes).ok())
        .filter(|s| s.schema_version == SAMPLES_SCHEMA_VERSION)
        .map(|s| s.samples)
        .unwrap_or_default()
}

/// Adds `report`'s reading to `space.json` and replaces the file
/// atomically. Two processes may race on it: the last writer wins.
pub fn record(env: &Env, report: &SpaceReport) -> io::Result<PathBuf> {
    let mut samples = read_samples(env);
    samples.push(Sample {
        at: report.checked_at.clone(),
        mount_point: report.volume.mount_point.clone(),
        total: report.volume.total_bytes,
        free: report.volume.free_bytes,
    });
    let samples = Samples { schema_version: SAMPLES_SCHEMA_VERSION, samples: thin(samples, env.now()) };
    let json = serde_json::to_vec_pretty(&samples).map_err(io::Error::other)?;
    store::write_atomic(&env.data_dir().join(SPACE), &json)
}

/// In time order: the first sample of each clock hour of the last
/// [`SAMPLES_KEPT`] before `now`, and the newest. A sample whose time cannot
/// be read, or that lies after `now` (a clock set back), leaves.
fn thin(samples: Vec<Sample>, now: SystemTime) -> Vec<Sample> {
    let now = unix_secs(now);
    let oldest = now.saturating_sub(SAMPLES_KEPT.as_secs());
    let mut timed: Vec<(u64, Sample)> = samples
        .into_iter()
        .filter_map(|s| Some((unix_secs(humantime::parse_rfc3339_weak(&s.at).ok()?), s)))
        .filter(|(t, _)| (oldest..=now).contains(t))
        .collect();
    timed.sort_by_key(|(t, _)| *t);
    let newest = timed.len().saturating_sub(1);
    let mut hour = None;
    let mut kept = Vec::new();
    for (i, (t, sample)) in timed.into_iter().enumerate() {
        if hour != Some(t / HOUR_SECS) || i == newest {
            hour = Some(t / HOUR_SECS);
            kept.push(sample);
        }
    }
    kept
}

fn unix_secs(t: SystemTime) -> u64 {
    t.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn rfc3339(t: SystemTime) -> String {
    humantime::format_rfc3339_seconds(t).to_string()
}
