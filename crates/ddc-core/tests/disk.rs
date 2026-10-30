//! `ddc disk` against fixture trees (story "Whole disk: what lies outside the
//! home folder"): the system's folders and tools live under a fixture root,
//! tools are fakes, and `statfs` puts everything on one Data volume.

mod common;

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use common::fakes::FakeCommands;
use common::{TreeBuilder, alloc, reference_alloc};
use ddc_core::disk::{
    self, AreaReveal, ContainerSource, DiskOptions, DiskPorts, DiskReport, HeldKind, HomeSource, InconsistentCause,
    SimulatorStatus, Sources,
};
use ddc_core::env::Env;
use ddc_core::report::volumes::StatFs;
use ddc_core::report::{self, ScanOptions};
use ddc_core::scan::{DirListing, DirReader, Progress, ReadError, StdReader};
use ddc_core::store;
use ddc_core::sys::statfs::VolumeStats;

const DATA: &str = "sys/System/Volumes/Data";

struct Fixture {
    t: TreeBuilder,
    now: SystemTime,
}

impl Fixture {
    fn new() -> Self {
        let t = TreeBuilder::new();
        t.dir("home");
        t.dir("data");
        t.dir(DATA);
        Self { t, now: SystemTime::now() }
    }

    fn env_at(&self, now: SystemTime) -> Env {
        Env::with_paths(&self.t.path("home"), &self.t.path("data"), &self.t.path("data/logs"), now).unwrap()
    }

    fn env(&self) -> Env {
        self.env_at(self.now)
    }

    fn data(&self, rel: &str) -> PathBuf {
        self.t.path(&format!("{DATA}/{rel}"))
    }

    fn file(&self, rel: &str, len: usize) -> PathBuf {
        self.t.file(&format!("{DATA}/{rel}"), len)
    }

    fn sources(&self) -> Sources {
        Sources::under(&self.t.path("sys"))
    }

    fn measure(&self, commands: &FakeCommands, opts: &DiskOptions) -> DiskReport {
        self.measure_with(commands, opts, &AtomicBool::new(false), None)
    }

    fn measure_with(
        &self,
        commands: &FakeCommands,
        opts: &DiskOptions,
        cancel: &AtomicBool,
        reader: Option<&dyn DirReader>,
    ) -> DiskReport {
        let ports = DiskPorts { commands, statfs: &OneVolume::DATA };
        let opts = DiskOptions { threads: Some(1), ..opts.clone() };
        disk::measure_with(&self.env(), &self.sources(), &opts, &ports, &Progress::default(), cancel, reader).unwrap()
    }
}

/// Every path on the volume mounted at `mount`, from `/dev/disk3s5`.
struct OneVolume {
    mount: &'static str,
}

impl OneVolume {
    const DATA: OneVolume = OneVolume { mount: "/System/Volumes/Data" };
    const OTHER: OneVolume = OneVolume { mount: "/Volumes/Two" };

    fn stats(mount: &str, from: &str) -> VolumeStats {
        VolumeStats {
            fs_type: "apfs".into(),
            mount_point: mount.into(),
            available_bytes: 200 << 30,
            total_bytes: 500 << 30,
            free_bytes: 200 << 30,
            mounted_from: from.into(),
        }
    }
}

impl StatFs for OneVolume {
    fn statfs(&self, _: &Path) -> io::Result<VolumeStats> {
        Ok(Self::stats(self.mount, "/dev/disk3s5"))
    }
    fn mounts(&self) -> io::Result<Vec<VolumeStats>> {
        Ok(vec![
            Self::stats("/", "/dev/disk3s1s1"),
            Self::stats("/System/Volumes/VM", "/dev/disk3s6"),
            Self::stats(self.mount, "/dev/disk3s5"),
        ])
    }
}

/// A startup container `disk3` whose Data volume consumes `data_used`, and a
/// simulator runtime's disk image in a container of its own.
fn apfs_plist(data_used: u64) -> String {
    let volume = |dev: &str, role: &str, used: u64| {
        let roles =
            if role.is_empty() { "<array/>".to_owned() } else { format!("<array><string>{role}</string></array>") };
        format!(
            "<dict><key>CapacityInUse</key><integer>{used}</integer><key>DeviceIdentifier</key><string>{dev}</string>\
             <key>Name</key><string>{role} volume</string><key>Roles</key>{roles}</dict>"
        )
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Containers</key><array>
<dict><key>CapacityCeiling</key><integer>18000000000</integer><key>CapacityFree</key><integer>500000000</integer>
<key>ContainerReference</key><string>disk8</string><key>Volumes</key><array>{}</array></dict>
<dict><key>CapacityCeiling</key><integer>1000000000000</integer><key>CapacityFree</key><integer>200000000000</integer>
<key>ContainerReference</key><string>disk3</string><key>Volumes</key><array>{}{}{}</array></dict>
</array></dict></plist>
"#,
        volume("disk8s1", "", 17_000_000_000),
        volume("disk3s1", "System", 12_000_000_000),
        volume("disk3s5", "Data", data_used),
        volume("disk3s6", "VM", 24_000_000_000),
    )
}

fn diskutil(data_used: u64) -> FakeCommands {
    FakeCommands::default().with("diskutil", true, &apfs_plist(data_used))
}

fn area<'r>(r: &'r DiskReport, id: &str) -> &'r disk::Area {
    r.areas.iter().find(|a| a.id == id).unwrap_or_else(|| panic!("no area {id}"))
}

fn scan_home(fx: &Fixture, env: &Env, roots: Vec<PathBuf>, statfs: &dyn StatFs) {
    let opts = ScanOptions { roots, ..ScanOptions::default() };
    let r = report::scan_with(env, &opts, &Progress::default(), statfs).unwrap();
    store::write_last_scan(env, &r).unwrap();
    assert!(fx.t.path("data/last-scan.json").exists());
}

#[test]
fn every_area_is_reported_with_its_bytes_and_a_folder_reached_twice_counts_once() {
    let fx = Fixture::new();
    let app = fx.file("Applications/Tool.app/Contents/MacOS/tool", 40_960);
    let lib = fx.file("Library/Fonts/font.ttf", 8192);
    let update = fx.file("Library/Updates/update.pkg", 16_384);
    let sim_image = fx.file("Library/Developer/CoreSimulator/Images/old.dmg", 12_288);
    let asset = fx.file("System/Library/AssetsV2/com_apple_MobileAsset_iOSSimulatorRuntime/a.asset/img", 20_480);
    let voice = fx.file("System/Library/AssetsV2/com_apple_MobileAsset_Voices/v.asset/v", 4096);
    let brew = fx.file("opt/homebrew/Cellar/jq/bin/jq", 8192);
    let conda = fx.file("opt/miniconda3/pkgs/numpy.tar", 8192);
    let other = fx.file("opt/other/bin/tool", 4096);
    let temp = fx.file("private/var/folders/ab/cdef/T/tmp.1", 4096);
    let tmp = fx.file("private/tmp/session/x", 4096);
    let sleep = fx.file("private/var/vm/sleepimage", 8192);
    let db = fx.file("private/var/db/x.db", 4096);
    let shared = fx.file("Users/Shared/shared.txt", 4096);
    let them = fx.file("Users/someone/notes.txt", 4096);
    let core = fx.file("cores/core.123", 4096);
    let home = fx.t.file("home/a.txt", 4096);
    // Like a firmlink: /usr/local reaches a folder another area already has.
    fx.t.dir(&format!("{DATA}/usr"));
    fx.t.symlink(fx.data("opt/other"), &format!("{DATA}/usr/local"));

    let r = fx.measure(&diskutil(1 << 40), &DiskOptions::default());

    assert_eq!(area(&r, "applications").bytes, alloc(&app));
    assert_eq!(area(&r, "library").bytes, alloc(&lib));
    assert_eq!(area(&r, "macos-update").bytes, alloc(&update));
    assert_eq!(area(&r, "macos-update").parent.as_deref(), Some("library"));
    assert_eq!(area(&r, "simulator-runtimes").bytes, alloc(&sim_image) + alloc(&asset));
    assert_eq!(area(&r, "simulator-runtimes").paths.len(), 2);
    assert_eq!(area(&r, "macos-assets").bytes, alloc(&voice));
    assert_eq!(area(&r, "homebrew").bytes, alloc(&brew));
    assert_eq!(area(&r, "conda").bytes, alloc(&conda));
    assert_eq!(area(&r, "opt").bytes, 0, "/opt/other is the usr-local region, the innermost one");
    assert_eq!(area(&r, "usr-local").bytes, alloc(&other));
    assert_eq!(area(&r, "temporary-folders").bytes, alloc(&temp));
    assert_eq!(area(&r, "tmp").bytes, alloc(&tmp));
    assert_eq!(area(&r, "sleep-image").bytes, alloc(&sleep));
    assert_eq!(area(&r, "private").bytes, alloc(&db));
    assert_eq!(area(&r, "shared").bytes, alloc(&shared));
    assert_eq!(area(&r, "users").bytes, alloc(&them));
    assert_eq!(area(&r, "cores").bytes, alloc(&core));
    assert!(r.areas.iter().all(|a| !a.partial), "{:?}", r.areas);
    assert_eq!(r.home.source, HomeSource::Walk);
    assert_eq!(r.home.bytes, alloc(&home));

    // Each inode once: what the areas and the home folder hold, nothing twice.
    let real: u64 = ["Applications", "Library", "System", "opt", "private", "Users", "cores"]
        .iter()
        .map(|d| reference_alloc(&fx.data(d)))
        .sum();
    assert_eq!(r.walk.bytes, real + alloc(&home));
    assert_eq!(r.not_counted.counted_bytes, r.walk.bytes);
    assert_eq!(r.areas.iter().map(|a| a.bytes).sum::<u64>() + r.home.bytes, r.walk.bytes);
}

#[test]
fn an_area_that_is_another_area_by_dev_and_ino_is_measured_once() {
    let fx = Fixture::new();
    let brew = fx.file("opt/homebrew/bin/brew", 8192);
    fx.t.dir(&format!("{DATA}/usr"));
    fx.t.symlink(fx.data("opt/homebrew"), &format!("{DATA}/usr/local"));
    let r = fx.measure(&diskutil(1 << 40), &DiskOptions::default());
    // The first area of the table naming the folder keeps it: here the root
    // `usr-local`, which comes before the nested `homebrew`.
    let named: Vec<&str> = r.areas.iter().filter(|a| a.bytes > 0).map(|a| a.id.as_str()).collect();
    assert_eq!(named, ["usr-local"], "{:?}", r.areas);
    assert!(r.areas.iter().all(|a| a.id != "homebrew"), "{:?}", r.areas);
    assert_eq!(area(&r, "usr-local").bytes, alloc(&brew));
    assert_eq!(r.walk.bytes, alloc(&brew), "counted once");
}

#[test]
fn the_home_row_comes_from_the_last_scan_only_when_it_still_describes_the_whole_home() {
    let fx = Fixture::new();
    fx.t.file("home/Code/app/main.rs", 8192);
    fx.t.file("home/notes.txt", 4096);
    fx.file("Applications/A.app/a", 4096);
    let home = fx.t.path("home");
    let commands = diskutil(1 << 40);

    // Whole home, on the Data volume, fresh.
    scan_home(&fx, &fx.env(), vec![home.clone()], &OneVolume::DATA);
    let r = fx.measure(&commands, &DiskOptions::default());
    assert_eq!(r.home.source, HomeSource::LastScan);
    assert_eq!(r.home.bytes, reference_alloc(&home));
    let scanned = store::read_last_scan(&fx.env()).unwrap().unwrap();
    assert_eq!(r.home.as_of, scanned.generated_at);
    assert_eq!(r.walk.bytes, alloc(&fx.data("Applications/A.app/a")), "the home folder was not walked");

    // Another schema version, or a cancelled scan.
    let path = fx.t.path("data/last-scan.json");
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for (key, value) in [
        ("schema_version", serde_json::json!(ddc_core::model::SCHEMA_VERSION + 1)),
        ("cancelled", serde_json::json!(true)),
    ] {
        let mut changed = saved.clone();
        changed[key] = value;
        std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert_eq!(fx.measure(&commands, &DiskOptions::default()).home.source, HomeSource::Walk, "{key}");
    }

    // ~/Code only.
    scan_home(&fx, &fx.env(), vec![home.join("Code")], &OneVolume::DATA);
    assert_eq!(fx.measure(&commands, &DiskOptions::default()).home.source, HomeSource::Walk);

    // A root on another volume.
    scan_home(&fx, &fx.env(), vec![home.clone()], &OneVolume::OTHER);
    assert_eq!(fx.measure(&commands, &DiskOptions::default()).home.source, HomeSource::Walk);

    // Two days old.
    let old = fx.env_at(fx.now - Duration::from_secs(2 * 86_400));
    scan_home(&fx, &old, vec![home.clone()], &OneVolume::DATA);
    let r = fx.measure(&commands, &DiskOptions::default());
    assert_eq!(r.home.source, HomeSource::Walk);
    assert_eq!(r.home.bytes, reference_alloc(&home));
    assert_eq!(r.home.as_of, r.generated_at);
}

#[test]
fn not_counted_is_consumed_minus_counted_and_never_clamped() {
    let fx = Fixture::new();
    fx.file("Library/a", 40_960);
    fx.t.file("home/b", 8192);
    let counted = fx.measure(&diskutil(1 << 40), &DiskOptions::default()).not_counted.counted_bytes;
    assert!(counted > 0);

    let r = fx.measure(&diskutil(counted + 1_000_000), &DiskOptions::default());
    assert_eq!(r.not_counted.data_volume_bytes, Some(counted + 1_000_000));
    assert_eq!(r.not_counted.bytes, Some(1_000_000));
    assert!(!r.not_counted.inconsistent);
    assert!(r.not_counted.causes.is_empty());

    let r = fx.measure(&diskutil(counted - 4096), &DiskOptions::default());
    assert_eq!(r.not_counted.bytes, Some(-4096));
    assert!(r.not_counted.inconsistent);
    assert_eq!(r.not_counted.causes, [InconsistentCause::ClonesCountedTwice]);

    // A home row from the last scan is the likelier cause.
    scan_home(&fx, &fx.env(), vec![fx.t.path("home")], &OneVolume::DATA);
    let r = fx.measure(&diskutil(4096), &DiskOptions::default());
    assert_eq!(r.home.source, HomeSource::LastScan);
    assert_eq!(r.not_counted.bytes, Some(4096 - i64::try_from(r.not_counted.counted_bytes).unwrap()));
    assert_eq!(r.not_counted.causes, [InconsistentCause::StaleHomeRow]);
}

#[test]
fn the_startup_container_is_the_one_holding_the_data_volume() {
    let fx = Fixture::new();
    let captured = include_str!("data/apfs-list.plist");
    let r = fx.measure(&FakeCommands::default().with("diskutil", true, captured), &DiskOptions::default());
    let c = r.container.unwrap();
    assert_eq!(c.source, ContainerSource::Diskutil);
    assert_eq!(c.reference.as_deref(), Some("disk3"));
    assert_eq!(c.capacity_bytes, 994_662_584_320);
    assert_eq!(c.in_use_bytes, 994_662_584_320 - 192_670_556_160);
    let roles: Vec<&str> = c.volumes.iter().map(|v| v.role.as_str()).collect();
    assert_eq!(roles, ["system", "preboot", "recovery", "update", "data", "vm"]);
    assert_eq!(r.not_counted.data_volume_bytes, Some(754_139_086_848));
    let system = &c.volumes[0];
    assert_eq!(system.mount_point.as_deref(), Some(Path::new("/")), "mounted from its snapshot");
    assert_eq!(c.volumes[2].mount_point, None, "Recovery is not mounted");
    assert!(c.volumes.iter().all(|v| v.used_bytes.is_some()));
}

#[test]
fn without_diskutil_the_volumes_come_from_statfs_and_recovery_is_not_measured() {
    let fx = Fixture::new();
    let r = fx.measure(&FakeCommands::default().with("diskutil", false, "Error"), &DiskOptions::default());
    let c = r.container.unwrap();
    assert_eq!(c.source, ContainerSource::Statfs);
    let data = c.volumes.iter().find(|v| v.role == "data").unwrap();
    assert_eq!(data.used_bytes, Some(300 << 30));
    assert!(c.volumes.iter().any(|v| v.role == "vm" && v.used_bytes.is_some()));
    assert!(c.volumes.iter().any(|v| v.role == "recovery" && v.used_bytes.is_none()));
    assert_eq!(r.not_counted.data_volume_bytes, Some(300 << 30));

    // No diskutil at all: the same.
    let r = fx.measure(&FakeCommands::default(), &DiskOptions::default());
    assert_eq!(r.container.unwrap().source, ContainerSource::Statfs);
}

#[test]
fn the_size_only_walk_produces_no_findings() {
    let fx = Fixture::new();
    let pyc = fx.file("opt/homebrew/lib/python3.13/site-packages/x/__pycache__/x.pyc", 8192);
    let module = fx.file("opt/homebrew/lib/node_modules/npm/index.js", 4096);
    let r = fx.measure(&diskutil(1 << 40), &DiskOptions::default());
    assert_eq!(area(&r, "homebrew").bytes, alloc(&pyc) + alloc(&module));
    let json = serde_json::to_string(&r).unwrap();
    assert!(!json.contains("__pycache__") && !json.contains("node_modules"), "{json}");
}

#[test]
fn the_entry_budget_stops_the_temporary_folders_and_marks_them_partial() {
    let fx = Fixture::new();
    for i in 0..30 {
        fx.file(&format!("private/var/folders/{i:02}/T/file"), 4096);
    }
    fx.file("private/tmp/one", 4096);
    let opts = DiskOptions { temp_entry_budget: 40, ..DiskOptions::default() };
    let r = fx.measure(&diskutil(1 << 40), &opts);
    let temp = area(&r, "temporary-folders");
    assert!(temp.partial);
    let read = temp.entries_read.unwrap();
    assert!(read >= 40, "{read}");
    assert!(temp.bytes < reference_alloc(&fx.data("private/var/folders")));
    let tmp = area(&r, "tmp");
    assert!(!tmp.partial);
    assert_eq!(tmp.entries_read, Some(1));
    assert!(!r.cancelled);
}

#[test]
fn a_folder_with_more_entries_than_the_budget_has_left_is_never_opened() {
    let fx = Fixture::new();
    for i in 0..50 {
        fx.file(&format!("private/var/folders/huge/{i}"), 4096);
    }
    let small = fx.file("private/var/folders/small/file", 4096);
    let opts = DiskOptions { temp_entry_budget: 20, ..DiskOptions::default() };
    let reader = Opened::default();
    let r = fx.measure_with(&diskutil(1 << 40), &opts, &AtomicBool::new(false), Some(&reader));
    let opened = reader.0.into_inner().unwrap();
    let huge = fx.data("private/var/folders/huge").canonicalize().unwrap();
    assert!(!opened.contains(&huge), "{opened:?}");
    let temp = area(&r, "temporary-folders");
    assert!(temp.partial);
    assert_eq!(temp.bytes, alloc(&small));
}

const RUNTIMES: &str = r#"{
  "AAAA-1111": {
    "identifier": "AAAA-1111",
    "runtimeIdentifier": "com.apple.CoreSimulator.SimRuntime.iOS-27-1",
    "platformIdentifier": "com.apple.platform.iphonesimulator",
    "version": "27.1", "build": "24A94425", "sizeBytes": 7000000000, "deletable": true, "state": "Ready",
    "path": "/System/Library/AssetsV2/com_apple_MobileAsset_iOSSimulatorRuntime/a.asset/AssetData",
    "parentMountPath": "/System/Library/AssetsV2/com_apple_MobileAsset_iOSSimulatorRuntime/a.asset/AssetData",
    "kind": "Patchable Cryptex Disk Image", "supportedArchitectures": ["arm64"]
  },
  "BBBB-2222": {
    "identifier": "BBBB-2222",
    "runtimeIdentifier": "com.apple.CoreSimulator.SimRuntime.iOS-27-0",
    "version": "27.0", "sizeBytes": 8000000000, "deletable": true, "state": "Ready"
  }
}"#;

const DEVICES: &str = r#"{"devices": {
  "com.apple.CoreSimulator.SimRuntime.iOS-27-1": [
    {"udid": "D1", "name": "iPhone A", "state": "Booted", "isAvailable": true, "dataPath": "/x"}
  ],
  "com.apple.CoreSimulator.SimRuntime.iOS-27-0": [
    {"udid": "D2", "name": "iPhone B", "state": "Shutdown", "isAvailable": true},
    {"udid": "D3", "name": "iPhone C", "state": "Shutdown", "isAvailable": true}
  ]
}}"#;

fn ran(commands: &FakeCommands, tool: &str) -> bool {
    commands.calls.borrow().iter().any(|c| c.split(' ').next() == Some(tool))
}

#[test]
fn simulator_runtimes_need_developer_tools_and_a_finished_first_launch() {
    let fx = Fixture::new();

    // No developer directory.
    let none = diskutil(1 << 40).with("xcode-select", false, "");
    let r = fx.measure(&none, &DiskOptions::default());
    assert!(r.simulators.is_none());
    assert!(!ran(&none, "simctl") && !ran(&none, "xcrun") && !ran(&none, "xcodebuild"));

    // Xcode's first launch is pending.
    let pending = diskutil(1 << 40)
        .with("xcode-select", true, "/Applications/Xcode.app/Contents/Developer\n")
        .with("xcodebuild", false, "")
        .with("simctl", true, RUNTIMES);
    let r = fx.measure(&pending, &DiskOptions::default());
    let s = r.simulators.unwrap();
    assert_eq!(s.status, SimulatorStatus::FirstLaunchPending);
    assert!(s.runtimes.is_empty());
    assert!(!ran(&pending, "simctl") && !ran(&pending, "xcrun"));
    assert!(pending.calls.borrow().contains(&"xcodebuild -checkFirstLaunchStatus".to_owned()));

    // Ready: runtimes with their devices, joined on the runtime identifier.
    let ready = diskutil(1 << 40)
        .with("xcode-select", true, "/Applications/Xcode.app/Contents/Developer\n")
        .with("xcodebuild", true, "")
        .answer("simctl", "runtime list -j", true, RUNTIMES)
        .answer("simctl", "list devices -j", true, DEVICES);
    let r = fx.measure(&ready, &DiskOptions::default());
    let s = r.simulators.unwrap();
    assert_eq!(s.status, SimulatorStatus::Listed);
    assert_eq!(s.listed_bytes, 15_000_000_000);
    let by_id = |id: &str| s.runtimes.iter().find(|r| r.identifier == id).unwrap();
    let new = by_id("AAAA-1111");
    assert!(new.booted);
    assert_eq!(new.devices.len(), 1);
    assert_eq!(new.version.as_deref(), Some("27.1"));
    assert!(new.parent_mount_path.as_ref().unwrap().starts_with("/System/Library/AssetsV2"));
    let old = by_id("BBBB-2222");
    assert!(!old.booted);
    assert_eq!(old.devices.len(), 2);
    assert!(!ran(&ready, "xcrun"));
    let envs = ready.envs.borrow();
    assert!(envs.iter().flatten().any(|e| e == "DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer"));
}

/// Sets the cancel flag at its `at`-th open.
struct CancelAt<'a> {
    opened: AtomicUsize,
    at: usize,
    cancel: &'a AtomicBool,
}

impl DirReader for CancelAt<'_> {
    fn read(&self, dir: &Path, expect: Option<(u64, u64)>, want_private: bool) -> Result<DirListing, ReadError> {
        if self.opened.fetch_add(1, Ordering::SeqCst) + 1 == self.at {
            self.cancel.store(true, Ordering::SeqCst);
        }
        StdReader.read(dir, expect, want_private)
    }
}

#[test]
fn a_stop_during_the_walk_still_reports_with_the_unfinished_areas_partial() {
    let fx = Fixture::new();
    for d in ["Applications", "Library", "opt/homebrew", "private/var/folders", "Users/Shared"] {
        for i in 0..5 {
            fx.file(&format!("{d}/{i}/f"), 4096);
        }
    }
    let cancel = AtomicBool::new(false);
    let reader = CancelAt { opened: AtomicUsize::new(0), at: 3, cancel: &cancel };
    let r = fx.measure_with(&diskutil(1 << 40), &DiskOptions::default(), &cancel, Some(&reader));
    assert!(r.cancelled);
    assert!(r.areas.iter().any(|a| a.partial), "{:?}", r.areas);
    assert!(r.areas.iter().any(|a| a.id == "homebrew" && a.partial));
    assert!(r.container.is_some(), "the tools ran before the stop");

    store::write_last_disk(&fx.env(), &r).unwrap();
    let saved: DiskReport = serde_json::from_slice(&std::fs::read(fx.t.path("data/last-disk.json")).unwrap()).unwrap();
    assert_eq!(saved, r);
}

#[test]
fn a_stop_before_the_tools_runs_none_of_them() {
    let fx = Fixture::new();
    fx.file("Library/f", 4096);
    let commands = diskutil(1 << 40).with("tmutil", true, "").with("xcode-select", true, "/Dev\n");
    let cancel = AtomicBool::new(true);
    let r = fx.measure_with(&commands, &DiskOptions::default(), &cancel, None);
    assert!(r.cancelled);
    assert!(commands.calls.borrow().is_empty());
    assert!(area(&r, "library").partial);
    assert!(r.home.partial);
}

/// Records every folder opened.
#[derive(Default)]
struct Opened(Mutex<Vec<PathBuf>>);

impl DirReader for Opened {
    fn read(&self, dir: &Path, expect: Option<(u64, u64)>, want_private: bool) -> Result<DirListing, ReadError> {
        self.0.lock().unwrap().push(dir.to_path_buf());
        StdReader.read(dir, expect, want_private)
    }
}

#[test]
fn the_home_walk_never_opens_a_skip_or_a_cloud_folder() {
    let fx = Fixture::new();
    fx.t.file("home/skipped/deep/secret", 4096);
    fx.t.file("home/Library/CloudStorage/Drive/doc", 4096);
    fx.t.file("home/Library/Mobile Documents/com~apple~CloudDocs/doc", 4096);
    let kept = fx.t.file("home/kept/file", 4096);
    let skip = fx.t.path("home/skipped");
    let opts = DiskOptions { user_skips: vec![skip.clone()], ..DiskOptions::default() };
    let reader = Opened::default();
    let r = fx.measure_with(&diskutil(1 << 40), &opts, &AtomicBool::new(false), Some(&reader));

    let cloud = fx.t.path("home/Library/CloudStorage");
    let docs = fx.t.path("home/Library/Mobile Documents");
    let opened = reader.0.into_inner().unwrap();
    assert!(opened.contains(&fx.t.path("home/kept")));
    for never in [&skip, &cloud, &docs] {
        assert!(!opened.iter().any(|p| p.starts_with(never)), "{never:?} opened: {opened:?}");
    }
    assert_eq!(r.home.source, HomeSource::Walk);
    assert_eq!(r.home.bytes, alloc(&kept));
    let held: Vec<(HeldKind, &Path)> = r.not_counted.holds.iter().map(|h| (h.kind, h.path.as_path())).collect();
    assert!(held.contains(&(HeldKind::Skip, skip.as_path())));
    assert!(held.contains(&(HeldKind::Cloud, cloud.as_path())));
    assert!(held.contains(&(HeldKind::Cloud, docs.as_path())));
}

#[test]
fn snapshots_are_counted_when_tmutil_answers() {
    let fx = Fixture::new();
    let list = "Snapshots for disk /:\ncom.apple.TimeMachine.2026-10-28-100000.local\ncom.apple.TimeMachine.2026-10-28-110000.local\n";
    let r = fx.measure(&diskutil(1 << 40).with("tmutil", true, list), &DiskOptions::default());
    assert_eq!(r.snapshots.count, Some(2));
    let r = fx.measure(&diskutil(1 << 40).with("tmutil", false, ""), &DiskOptions::default());
    assert_eq!(r.snapshots.count, None);
    let r = fx.measure(&diskutil(1 << 40), &DiskOptions::default());
    assert_eq!(r.snapshots.count, None, "no tmutil: unknown, not zero");
}

/// Story "Whole disk: Show in Finder and the honest action for each row":
/// the table's action for each row. A new area must be added here too.
#[test]
fn every_area_has_the_reveal_of_the_story_table() {
    use AreaReveal::{Finder, SoftwareUpdate};
    let table: &[(&str, Option<AreaReveal>)] = &[
        ("applications", Some(Finder)),
        ("library", Some(Finder)),
        ("macos-assets", None),
        ("opt", Some(Finder)),
        ("usr-local", Some(Finder)),
        ("private", None),
        ("users", None),
        ("cores", Some(Finder)),
        ("macos-update", Some(SoftwareUpdate)),
        ("simulator-runtimes", Some(Finder)),
        ("homebrew", Some(Finder)),
        ("conda", Some(Finder)),
        ("temporary-folders", None),
        ("tmp", None),
        ("sleep-image", None),
        ("shared", Some(Finder)),
    ];
    let mut ids: Vec<&str> = disk::AREAS.iter().map(|d| d.id).collect();
    let mut named: Vec<&str> = table.iter().map(|(id, _)| *id).collect();
    ids.sort_unstable();
    named.sort_unstable();
    assert_eq!(ids, named, "every area of the table, and no other");
    for (id, reveal) in table {
        let def = disk::AREAS.iter().find(|d| d.id == *id).unwrap();
        assert_eq!(def.reveal, *reveal, "{id}");
    }
}

#[test]
fn the_report_carries_each_areas_reveal() {
    let fx = Fixture::new();
    fx.file("Applications/App.app/x", 4096);
    fx.file("Library/Updates/update.pkg", 4096);
    fx.file("private/etc/x", 4096);
    let r = fx.measure(&diskutil(1 << 40), &DiskOptions::default());
    let json = serde_json::to_value(&r).unwrap();
    let reveal = |id: &str| {
        let areas = json["areas"].as_array().unwrap();
        areas.iter().find(|a| a["id"] == id).unwrap().get("reveal").cloned()
    };
    assert_eq!(reveal("applications"), Some(serde_json::json!("finder")));
    assert_eq!(reveal("macos-update"), Some(serde_json::json!("software-update")));
    assert_eq!(reveal("private"), None, "no key for a row with nothing to press");
}

#[test]
fn the_disk_schema_names_area_reveal() {
    let schema = serde_json::to_value(schemars::schema_for!(DiskReport)).unwrap();
    let def = &schema["$defs"]["AreaReveal"];
    // `enum: [...]`, or `oneOf` with a `const` per documented variant.
    fn values(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::Object(map) => {
                for (key, value) in map {
                    match (key.as_str(), value) {
                        ("enum", serde_json::Value::Array(items)) => {
                            out.extend(items.iter().filter_map(|i| i.as_str().map(str::to_owned)))
                        }
                        ("const", serde_json::Value::String(s)) => out.push(s.clone()),
                        _ => values(value, out),
                    }
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|i| values(i, out)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    values(def, &mut found);
    found.sort_unstable();
    assert_eq!(found, ["finder", "software-update"], "{def}");
}

#[test]
#[allow(clippy::disallowed_methods)] // DDC_UPDATE_SCHEMA regenerates the committed file
fn committed_disk_schema_is_current() {
    let schema = serde_json::to_string_pretty(&schemars::schema_for!(DiskReport)).unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/disk.schema.json");
    if std::env::var_os("DDC_UPDATE_SCHEMA").is_some() {
        std::fs::write(&path, &schema).unwrap();
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(committed == schema, "docs/disk.schema.json is stale: run DDC_UPDATE_SCHEMA=1 cargo test");
}

#[test]
fn a_skip_outside_the_home_folder_is_never_opened_through_the_data_volume() {
    let fx = Fixture::new();
    fx.file("opt/private-tool/secret", 4096);
    let kept = fx.file("opt/other/file", 4096);
    // Skips are named through `/`, as `Settings::resolve` gives them.
    let opts = DiskOptions { user_skips: vec![PathBuf::from("/opt/private-tool")], ..DiskOptions::default() };
    let reader = Opened::default();
    let r = fx.measure_with(&diskutil(1 << 40), &opts, &AtomicBool::new(false), Some(&reader));
    let opened = reader.0.into_inner().unwrap();
    let skipped = fx.data("opt/private-tool").canonicalize().unwrap();
    assert!(!opened.iter().any(|p| p.starts_with(&skipped)), "{opened:?}");
    assert_eq!(area(&r, "opt").bytes, alloc(&kept));
}

#[test]
fn the_apps_disk_fixtures_are_reports_the_cli_could_write() {
    // Written by hand for the app's tests (story "Whole disk in the app"):
    // each one reads as a `DiskReport` and writes back to the same JSON, so
    // it holds no field the CLI does not write.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/Packages/DDCKit/Tests/DDCKitTests/Fixtures");
    for name in ["disk-report.json", "disk-cancelled.json"] {
        let text = std::fs::read_to_string(dir.join(name)).unwrap();
        let json: serde_json::Value = serde_json::from_str(&text).unwrap();
        let r: DiskReport = serde_json::from_value(json.clone()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(r.schema_version, disk::DISK_SCHEMA_VERSION, "{name}");
        assert_eq!(serde_json::to_value(&r).unwrap(), json, "{name}");
    }
}

// Story "Whole disk: Conda environments and its package cache, with conda":
// the `/opt` installs' environments and package folder, from the file
// system alone.

/// `/opt/miniconda3` in the Data volume, reached through the fixture's
/// `/opt` link: the environment `old` with a file of its own and one
/// hard-linked from `pkgs`, the environment `ml`, a folder in `envs` that
/// is no environment, and `pkgs` with a package of its own.
fn conda_fixture() -> (Fixture, [PathBuf; 4]) {
    let fx = Fixture::new();
    fx.t.symlink("System/Volumes/Data/opt", "sys/opt");
    fx.t.write(&format!("{DATA}/opt/miniconda3/conda-meta/history"), "");
    fx.t.write(&format!("{DATA}/opt/miniconda3/bin/conda"), "#!/bin/sh\nexit 1\n");
    fx.t.write(&format!("{DATA}/opt/miniconda3/envs/old/conda-meta/history"), "");
    fx.t.write(&format!("{DATA}/opt/miniconda3/envs/ml/conda-meta/history"), "");
    fx.t.dir(&format!("{DATA}/opt/miniconda3/envs/not-one"));
    let own = fx.file("opt/miniconda3/envs/old/lib/own.dylib", 64 * 1024);
    let linked = fx.file("opt/miniconda3/pkgs/numpy-1.26/lib/numpy.so", 128 * 1024);
    fx.t.hardlink(&linked, &format!("{DATA}/opt/miniconda3/envs/old/lib/numpy.so"));
    let unused = fx.file("opt/miniconda3/pkgs/scipy-1.11/lib/scipy.so", 32 * 1024);
    let ml = fx.file("opt/miniconda3/envs/ml/lib/torch.dylib", 16 * 1024);
    (fx, [own, linked, unused, ml])
}

#[test]
fn each_opt_environment_and_the_package_cache_are_measured_and_links_between_them_are_shared() {
    let (fx, [own, linked, unused, ml]) = conda_fixture();
    let commands = diskutil(1 << 40);

    let r = fx.measure(&commands, &DiskOptions::default());

    let conda = r.conda.as_ref().expect("a conda section");
    assert_eq!(conda.installs.len(), 1);
    let install = &conda.installs[0];
    let prefix = fx.sources().opt.join("miniconda3");
    assert_eq!(install.prefix, prefix, "spelled through /opt, as conda spells it");
    let names: Vec<PathBuf> = install.environments.iter().map(|e| e.path.clone()).collect();
    assert_eq!(names, [prefix.join("envs/ml"), prefix.join("envs/old")]);
    let old = &install.environments[1];
    assert_eq!(old.bytes, alloc(&own));
    assert_eq!(old.shared_bytes, alloc(&linked));
    assert!(old.writable && !old.partial);
    assert_eq!(install.environments[0].bytes, alloc(&ml));
    let pkgs = install.packages.as_ref().expect("the package folder");
    assert_eq!(pkgs.path, prefix.join("pkgs"));
    assert_eq!(pkgs.bytes, alloc(&unused));
    assert_eq!(pkgs.shared_bytes, alloc(&linked));
    assert!(pkgs.writable);
    // The area keeps its own numbers: each file once.
    assert_eq!(area(&r, "conda").bytes, reference_alloc(&fx.data("opt/miniconda3")));
    assert!(!commands.calls.borrow().iter().any(|c| c.starts_with("conda")), "{:?}", commands.calls.borrow());
}

#[test]
fn a_folder_the_user_cannot_write_is_marked_for_an_administrator() {
    use std::os::unix::fs::PermissionsExt as _;
    let (fx, _) = conda_fixture();
    let envs = fx.data("opt/miniconda3/envs");
    let pkgs = fx.data("opt/miniconda3/pkgs");
    std::fs::set_permissions(&envs, std::fs::Permissions::from_mode(0o555)).unwrap();
    std::fs::set_permissions(&pkgs, std::fs::Permissions::from_mode(0o555)).unwrap();
    let r = fx.measure(&diskutil(1 << 40), &DiskOptions::default());
    std::fs::set_permissions(&envs, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(&pkgs, std::fs::Permissions::from_mode(0o755)).unwrap();

    let install = &r.conda.as_ref().unwrap().installs[0];
    assert!(install.environments.iter().all(|e| !e.writable), "{install:?}");
    assert!(!install.packages.as_ref().unwrap().writable);
}

#[test]
fn without_an_opt_install_there_is_no_conda_section() {
    let fx = Fixture::new();
    fx.t.symlink("System/Volumes/Data/opt", "sys/opt");
    fx.file("opt/homebrew/bin/brew", 4096);
    let r = fx.measure(&diskutil(1 << 40), &DiskOptions::default());
    assert!(r.conda.is_none());
    assert!(serde_json::to_value(&r).unwrap().get("conda").is_none());
}
