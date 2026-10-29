//! `last-disk.json` and `ddc disk --json` (schema `docs/disk.schema.json`).

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Version of [`DiskReport`]; the scan report has its own. A new enum
/// variant needs a bump: the app decodes strictly, except [`AreaReveal`],
/// which it reads leniently (an unknown value is no button).
pub const DISK_SCHEMA_VERSION: u32 = 2;

/// What uses the startup disk, the home folder and everything outside it
/// (story "Whole disk: what lies outside the home folder"). Read-only:
/// nothing in it is a finding, and nothing is selected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DiskReport {
    pub schema_version: u32,
    pub ddc_version: String,
    /// RFC 3339, UTC.
    pub generated_at: String,
    pub duration_ms: u64,
    /// Stopped by a signal: the areas not finished are `partial`.
    pub cancelled: bool,
    /// The APFS container of the startup disk. Absent when neither
    /// `diskutil` nor `statfs` could read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<Container>,
    pub home: HomeRow,
    /// The areas outside the home folder that exist on this Mac, in the
    /// table's order.
    pub areas: Vec<Area>,
    /// Absent without developer tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simulators: Option<Simulators>,
    /// The conda installs in `/opt`, from the file system; absent when
    /// there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conda: Option<Conda>,
    pub snapshots: Snapshots,
    /// What this run's walk read, each inode once.
    pub walk: WalkTotals,
    pub not_counted: NotCounted,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WalkTotals {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ContainerSource {
    /// `diskutil apfs list -plist`.
    Diskutil,
    /// `statfs` per mounted volume: unmounted volumes are not measured.
    Statfs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Container {
    pub source: ContainerSource,
    /// `disk3`; absent from `statfs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// The container's size; from `statfs`, what it uses plus the Data
    /// volume's free bytes.
    pub capacity_bytes: u64,
    /// What Disk Utility calls used.
    pub in_use_bytes: u64,
    pub volumes: Vec<VolumeRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VolumeRow {
    pub name: String,
    /// The APFS role, lowercase: `system`, `data`, `vm`, `preboot`,
    /// `recovery`, `update`, `xart`, `hardware`, or `none`.
    pub role: String,
    /// `disk3s5`; absent when not known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// Bytes the volume consumes; absent when not measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount_point: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum HomeSource {
    /// `last-scan.json`: whole home, current schema, on the Data volume, at
    /// most a day old.
    LastScan,
    /// Measured by this run, without opening the cloud folders or the skips.
    Walk,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HomeRow {
    pub path: PathBuf,
    pub source: HomeSource,
    /// RFC 3339: the last scan's `generated_at`, or this report's.
    pub as_of: String,
    /// Allocated bytes, a hard link shared with an area included, as the
    /// scan's `scanned_bytes` counts them.
    pub bytes: u64,
    /// Bytes the scan could free, from `last-scan.json`'s classes; absent
    /// for a walk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reclaimable_bytes: Option<u64>,
    /// Folders that could not be read.
    pub unreadable: u64,
    /// The walk stopped before it finished.
    pub partial: bool,
}

/// The one-click tool an area can be freed with (story "Whole disk: free
/// space with the owning tool"); this report only names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ActionId {
    SimctlRuntimeDelete,
    BrewCleanup,
    /// `conda env remove -p` of one `/opt` environment; its identifier is
    /// the environment's path.
    CondaEnvRemove,
    /// `conda clean` of one `/opt` install's package folder; its
    /// identifier is the install's prefix.
    CondaClean,
}

/// Where the app sends the user for an area it does not free itself
/// (story "Whole disk: Show in Finder and the honest action for each row").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum AreaReveal {
    /// Show in Finder: the area's folders, by their firmlinked paths.
    Finder,
    /// Open Software Update in System Settings.
    SoftwareUpdate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Area {
    /// Stable id from the table in code (`applications`, `homebrew`, …).
    pub id: String,
    /// The paths measured, through `/System/Volumes/Data`.
    pub paths: Vec<PathBuf>,
    /// The area inside which this one lies; its bytes are not in that one's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub title: String,
    /// What it is.
    pub explanation: String,
    /// What frees it.
    pub frees: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<ActionId>,
    /// What the app offers besides the tool's action; absent for an area
    /// that has nothing to press.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reveal: Option<AreaReveal>,
    /// Allocated bytes that belong to this area alone, each inode once.
    pub bytes: u64,
    /// Bytes of hard-linked files that also have links outside this area.
    pub shared_bytes: u64,
    pub files: u64,
    pub dirs: u64,
    /// Folders and entries that could not be read.
    pub unreadable: u64,
    /// Not walked to the end (the entry budget, or a stop): `bytes` is a
    /// lower bound.
    pub partial: bool,
    /// Entries read, for an area walked under the entry budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entries_read: Option<u64>,
    /// An area action ran on it after this report was measured (`ddc disk
    /// free`): its numbers are from before. Absent when false.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SimulatorStatus {
    /// The runtimes were listed.
    Listed,
    /// `xcodebuild -checkFirstLaunchStatus` failed: listing them would
    /// install components with an administrator's password.
    FirstLaunchPending,
    /// `simctl` failed or printed what could not be read.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Simulators {
    pub status: SimulatorStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub runtimes: Vec<Runtime>,
    /// The runtimes' `sizeBytes` summed; compare with the
    /// `simulator-runtimes` area's bytes, which also hold assets no runtime
    /// lists.
    pub listed_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Runtime {
    /// The id `simctl runtime delete` takes.
    pub identifier: String,
    /// `com.apple.CoreSimulator.SimRuntime.iOS-27-1`, the key devices are
    /// grouped under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_identifier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_mount_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletable: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    pub devices: Vec<Device>,
    /// A device on this runtime is running.
    pub booted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Device {
    pub udid: String,
    pub name: String,
    pub state: String,
}

/// The `/opt` conda installs (story "Whole disk: Conda environments and its
/// package cache, with conda"). Read from the file system: `ddc disk`
/// never runs conda.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Conda {
    /// In the order `miniconda3`, `anaconda3`, `miniforge3`.
    pub installs: Vec<CondaInstall>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CondaInstall {
    /// Through the firmlink, as conda spells it: `/opt/miniconda3`. The
    /// identifier of its `conda-clean`.
    pub prefix: PathBuf,
    /// `<prefix>/pkgs`; absent when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packages: Option<CondaFolder>,
    /// Every `<prefix>/envs/*` holding `conda-meta/history`, by path. Each
    /// path is the identifier of its `conda-env-remove`.
    pub environments: Vec<CondaFolder>,
}

/// An environment or a package folder, measured as one region of a walk of
/// the install's environments and package folder together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CondaFolder {
    /// Through the firmlink, as conda spells it.
    pub path: PathBuf,
    /// Allocated bytes of the files whose every link lies inside it: what
    /// removing it alone frees.
    pub bytes: u64,
    /// Bytes of hard-linked files with a link elsewhere too, such as an
    /// environment's files linked from the package folder.
    pub shared_bytes: u64,
    pub files: u64,
    pub unreadable: u64,
    /// Not walked to the end: the numbers are lower bounds.
    pub partial: bool,
    /// The user running ddc may write it (and, for an environment, the
    /// `envs` folder holding it): without that, conda needs an administrator.
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Snapshots {
    /// Local Time Machine snapshots of `/`; absent when `tmutil` could not
    /// tell. Their size cannot be measured without root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum InconsistentCause {
    /// The home row came from the last scan, and the home folder has grown
    /// or shrunk since.
    StaleHomeRow,
    /// APFS clones are counted once on each side, but stored once.
    ClonesCountedTwice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum HeldKind {
    /// A cloud folder: reading it would download it.
    Cloud,
    /// A folder in `settings.json`'s `skips`.
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Held {
    pub kind: HeldKind,
    pub path: PathBuf,
}

/// The Data volume's consumed bytes minus everything counted on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct NotCounted {
    /// Signed and never clamped; absent when the Data volume was not measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data_volume_bytes: Option<u64>,
    /// The home row plus every area, each inode once.
    pub counted_bytes: u64,
    /// More was counted than the volume holds.
    pub inconsistent: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub causes: Vec<InconsistentCause>,
    /// What the walk did not open, by name.
    pub holds: Vec<Held>,
    /// Folders no walk of this report could read.
    pub unreadable: u64,
}
