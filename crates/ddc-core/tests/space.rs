//! `ddc space`'s rule and its files (story "Popover: say when used space
//! grew since the last scan"; ADR "Menu bar app: Swift shell over the ddc
//! CLI", §6 item 13): a fake `statfs`, a saved report's head written by hand
//! in a temporary data folder, and `space.json`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use ddc_core::env::Env;
use ddc_core::report::volumes::{StatFs, VolumeStats};
use ddc_core::space::{
    self, Baseline, BaselineSource, SPACE_SCHEMA_VERSION, Saved, SpaceReport, SpaceState, SpaceVolume,
};
use serde_json::{Value, json};

const G: u64 = 1 << 30;
const DATA: &str = "/System/Volumes/Data";
/// 2026-11-03T10:00:00Z, on the hour.
const NOW: u64 = 1_793_700_000;
const SCANNED: &str = "2026-11-02T08:00:00Z";

/// The home's volume at [`DATA`]: `total` and `free` bytes.
struct Volume(u64, u64);

impl StatFs for Volume {
    fn statfs(&self, _: &Path) -> io::Result<VolumeStats> {
        Ok(VolumeStats {
            fs_type: "apfs".into(),
            mount_point: DATA.into(),
            available_bytes: self.1,
            total_bytes: self.0,
            // `f_bfree`, the root's reserve included: never what `space` reads.
            free_bytes: self.1 + G,
            mounted_from: "/dev/disk3s5".into(),
        })
    }
}

struct Unreadable;

impl StatFs for Unreadable {
    fn statfs(&self, _: &Path) -> io::Result<VolumeStats> {
        Err(io::Error::other("gone"))
    }
}

struct Dirs {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    data: PathBuf,
}

impl Dirs {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        fs::create_dir(&home).unwrap();
        let data = tmp.path().join("data");
        fs::create_dir(&data).unwrap();
        Self { _tmp: tmp, home, data }
    }

    fn env_at(&self, secs: u64) -> Env {
        Env::with_paths(&self.home, &self.data, &self.data.join("logs"), UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap()
    }

    fn env(&self) -> Env {
        self.env_at(NOW)
    }

    /// `last-scan.json` as `report`.
    fn save(&self, report: &Value) {
        fs::write(self.data.join("last-scan.json"), serde_json::to_vec_pretty(report).unwrap()).unwrap();
    }

    fn check(&self, statfs: &dyn StatFs, low_disk: bool) -> SpaceReport {
        space::check(&self.env(), statfs, low_disk).unwrap()
    }

    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> =
            fs::read_dir(&self.data).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }
}

/// The head of a saved report: its time and one volume per `(mount, total,
/// free, read_at)`, with the fields a scan writes around them.
fn report(volumes: &[(&str, u64, u64, Option<&str>)]) -> Value {
    let volumes: Vec<Value> = volumes
        .iter()
        .map(|(mount, total, free, read_at)| {
            let mut v =
                json!({ "mount_point": mount, "roots": ["/Users/me"], "free_bytes": free, "total_bytes": total });
            if let Some(at) = read_at {
                v["read_at"] = json!(at);
            }
            v
        })
        .collect();
    json!({
        "schema_version": ddc_core::model::SCHEMA_VERSION,
        "generated_at": SCANNED,
        "cancelled": false,
        "volumes": volumes,
        "findings": [{ "path": "/Users/me/x", "bytes": 1 }],
    })
}

/// A saved scan of [`DATA`] with `free` of `total` bytes free.
fn scanned(total: u64, free: u64) -> Value {
    report(&[(DATA, total, free, None)])
}

#[test]
fn the_threshold_is_five_percent_of_the_volume_with_20_gib_at_least() {
    let d = Dirs::new();
    // 500 GiB: 5 % is 25 GiB.
    d.save(&scanned(500 * G, 300 * G));
    let r = d.check(&Volume(500 * G, 276 * G), true);
    assert_eq!((r.state, r.grew, r.in_use_delta), (SpaceState::Quiet, false, Some((24 * G) as i64)));
    let r = d.check(&Volume(500 * G, 275 * G), true);
    assert_eq!((r.state, r.grew, r.in_use_delta), (SpaceState::Grew, true, Some((25 * G) as i64)));
    let r = d.check(&Volume(500 * G, 275 * G - 1), true);
    assert_eq!((r.state, r.grew), (SpaceState::Grew, true));
    // 100 GiB: 5 % is 5 GiB, under the 20 GiB floor.
    d.save(&scanned(100 * G, 60 * G));
    let r = d.check(&Volume(100 * G, 41 * G), true);
    assert_eq!((r.state, r.grew, r.in_use_delta), (SpaceState::Quiet, false, Some((19 * G) as i64)));
    let r = d.check(&Volume(100 * G, 40 * G), true);
    assert_eq!((r.state, r.grew, r.in_use_delta), (SpaceState::Grew, true, Some((20 * G) as i64)));
    assert_eq!(space::grew_threshold(100 * G), 20 * G);
    assert_eq!(space::grew_threshold(2_000 * G), 100 * G);
    // More free space than at the scan: never fires.
    let r = d.check(&Volume(100 * G, 90 * G), true);
    assert_eq!((r.state, r.grew, r.in_use_delta), (SpaceState::Quiet, false, Some(-(30 * G as i64))));
}

#[test]
fn under_ten_percent_free_is_low_disk_whatever_the_delta_and_grew_is_still_said() {
    let d = Dirs::new();
    let low = Volume(500 * G, 49 * G);
    for (base_free, delta, grew) in [(49, 0, false), (30, -19, false), (60, 11, false), (74, 25, true), (90, 41, true)]
    {
        d.save(&scanned(500 * G, base_free * G));
        let r = d.check(&low, true);
        assert_eq!(r.state, SpaceState::LowDisk, "baseline {base_free} GiB free");
        assert_eq!((r.grew, r.in_use_delta), (grew, Some(delta * G as i64)), "baseline {base_free} GiB free");
        // `notify_low_disk: false`: growth or nothing.
        let off = d.check(&low, false);
        let want = if grew { SpaceState::Grew } else { SpaceState::Quiet };
        assert_eq!((off.state, off.grew), (want, grew), "baseline {base_free} GiB free");
    }
    // Exactly 10 % free is not under it.
    d.save(&scanned(500 * G, 50 * G));
    assert_eq!(d.check(&Volume(500 * G, 50 * G), true).state, SpaceState::Quiet);
    // An empty volume total never divides.
    assert_eq!(d.check(&Volume(0, 0), true).state, SpaceState::Quiet);
}

#[test]
fn no_saved_report_or_none_of_the_homes_volume_gives_no_baseline() {
    let d = Dirs::new();
    let r = d.check(&Volume(500 * G, 200 * G), true);
    assert_eq!((r.baseline.as_ref(), r.in_use_delta, r.grew, r.state), (None, None, false, SpaceState::Quiet));
    let r = d.check(&Volume(500 * G, 20 * G), true);
    assert_eq!((r.baseline.as_ref(), r.in_use_delta, r.grew, r.state), (None, None, false, SpaceState::LowDisk));
    assert_eq!(d.check(&Volume(500 * G, 20 * G), false).state, SpaceState::Quiet);
    // Only other volumes: none of them is the baseline.
    d.save(&report(&[("/", 500 * G, 400 * G, None), ("/Volumes/Work", 500 * G, 400 * G, None)]));
    let r = d.check(&Volume(500 * G, 20 * G), true);
    assert_eq!((r.baseline.as_ref(), r.in_use_delta, r.state), (None, None, SpaceState::LowDisk));
    // The home's among others: only it counts.
    d.save(&report(&[("/Volumes/Work", 500 * G, 1, None), (DATA, 500 * G, 210 * G, None), ("/", 10, 1, None)]));
    let r = d.check(&Volume(500 * G, 200 * G), true);
    assert_eq!((r.baseline.unwrap().free_bytes, r.in_use_delta), (210 * G, Some((10 * G) as i64)));
    // Not JSON, or JSON without the head: no baseline either.
    for text in ["{ not json", "[]", r#"{"generated_at": "2026-11-02T08:00:00Z"}"#, r#"{"volumes": []}"#] {
        fs::write(d.data.join("last-scan.json"), text).unwrap();
        assert_eq!(d.check(&Volume(500 * G, 200 * G), true).baseline, None, "{text}");
    }
}

#[test]
fn a_report_of_an_older_schema_version_or_a_cancelled_one_still_serves() {
    let d = Dirs::new();
    let mut old = scanned(500 * G, 300 * G);
    old["schema_version"] = json!(3);
    old["cancelled"] = json!(true);
    old["totals"] = json!({ "files": 5 });
    d.save(&old);
    let r = d.check(&Volume(500 * G, 200 * G), true);
    assert_eq!(r.baseline, Some(Baseline { source: BaselineSource::Scan, at: SCANNED.into(), free_bytes: 300 * G }));
    assert_eq!((r.state, r.in_use_delta), (SpaceState::Grew, Some((100 * G) as i64)));
}

#[test]
fn the_baseline_is_the_scan_or_the_clean_that_read_the_free_space() {
    let d = Dirs::new();
    d.save(&scanned(500 * G, 300 * G));
    let r = d.check(&Volume(500 * G, 300 * G), true);
    assert_eq!(r.baseline, Some(Baseline { source: BaselineSource::Scan, at: SCANNED.into(), free_bytes: 300 * G }));
    // A `clean --items` edit read the free space again: its time, copied as written.
    let read_at = "2026-11-03T09:30:00.250Z";
    d.save(&report(&[("/", 1, 1, Some("2026-11-03T09:00:00Z")), (DATA, 500 * G, 330 * G, Some(read_at))]));
    let r = d.check(&Volume(500 * G, 300 * G), true);
    assert_eq!(r.baseline, Some(Baseline { source: BaselineSource::Clean, at: read_at.into(), free_bytes: 330 * G }));
    assert_eq!((r.state, r.in_use_delta), (SpaceState::Grew, Some((30 * G) as i64)));
    assert_eq!(r.checked_at, "2026-11-03T10:00:00Z");
    assert_eq!(r.volume, SpaceVolume { mount_point: DATA.into(), total_bytes: 500 * G, free_bytes: 300 * G });
}

#[test]
fn a_volume_that_cannot_be_read_is_an_error() {
    let d = Dirs::new();
    let e = space::check(&d.env(), &Unreadable, true).unwrap_err();
    assert_eq!(e.to_string(), "gone");
}

#[test]
fn the_json_writes_its_absent_values_as_null_and_its_states_in_kebab_case() {
    let low = space::decide("2026-11-03T10:00:00Z".into(), volume(500 * G, 20 * G), None, true);
    assert_eq!(
        serde_json::to_value(&low).unwrap(),
        json!({
            "schema_version": SPACE_SCHEMA_VERSION, "checked_at": "2026-11-03T10:00:00Z",
            "volume": { "mount_point": DATA, "total_bytes": 500 * G, "free_bytes": 20 * G },
            "baseline": null, "in_use_delta": null, "grew": false, "state": "low-disk",
        })
    );
    let clean = Saved {
        baseline: Baseline { source: BaselineSource::Clean, at: "2026-11-03T09:00:00Z".into(), free_bytes: 300 * G },
        total_bytes: 500 * G,
    };
    let grew = space::decide("2026-11-03T10:00:00Z".into(), volume(500 * G, 200 * G), Some(clean), true);
    let json = serde_json::to_value(&grew).unwrap();
    assert_eq!(json["baseline"], json!({ "source": "clean", "at": "2026-11-03T09:00:00Z", "free_bytes": 300 * G }));
    assert_eq!((json["state"].as_str(), json["grew"].as_bool()), (Some("grew"), Some(true)));
    for r in [low, grew] {
        let back: SpaceReport = serde_json::from_value(serde_json::to_value(&r).unwrap()).unwrap();
        assert_eq!(back, r);
    }
}

fn volume(total: u64, free: u64) -> SpaceVolume {
    SpaceVolume { mount_point: DATA.into(), total_bytes: total, free_bytes: free }
}

/// `space.json`'s times.
fn sample_times(d: &Dirs) -> Vec<String> {
    let json: Value = serde_json::from_slice(&fs::read(d.data.join("space.json")).unwrap()).unwrap();
    assert_eq!(json["schema_version"], 1);
    json["samples"].as_array().unwrap().iter().map(|s| s["at"].as_str().unwrap().to_owned()).collect()
}

fn record_at(d: &Dirs, secs: u64, free: u64) {
    let env = d.env_at(secs);
    let r = space::check(&env, &Volume(500 * G, free), true).unwrap();
    space::record(&env, &r).unwrap();
}

fn rfc3339(secs: u64) -> String {
    humantime::format_rfc3339_seconds(UNIX_EPOCH + Duration::from_secs(secs)).to_string()
}

#[test]
fn space_json_keeps_the_first_sample_of_each_hour_for_14_days_and_the_newest() {
    let d = Dirs::new();
    const H: u64 = 3_600;
    record_at(&d, NOW - 15 * 86_400, 1);
    record_at(&d, NOW - 14 * 86_400 + 60, 2);
    record_at(&d, NOW - 2 * H, 3);
    record_at(&d, NOW - 2 * H + 600, 4);
    assert_eq!(
        sample_times(&d),
        [rfc3339(NOW - 14 * 86_400 + 60), rfc3339(NOW - 2 * H), rfc3339(NOW - 2 * H + 600)],
        "within 14 days of the newest: each first of its hour, and the newest"
    );
    record_at(&d, NOW - H + 300, 5);
    record_at(&d, NOW - H + 1_200, 6);
    record_at(&d, NOW + 60, 7);
    record_at(&d, NOW + 1_800, 8);
    assert_eq!(
        sample_times(&d),
        [rfc3339(NOW - 2 * H), rfc3339(NOW - H + 300), rfc3339(NOW + 60), rfc3339(NOW + 1_800)],
        "a newest that is not the first of its hour leaves once another comes"
    );
    let samples = space::read_samples(&d.env());
    assert_eq!(samples.last().map(|s| (s.mount_point.clone(), s.total, s.free)), Some((DATA.into(), 500 * G, 8)));
    // 14 days on, only what is newer than 14 days stays, and 10:30 is no
    // longer the newest.
    record_at(&d, NOW + 14 * 86_400 - 2 * H + 1, 9);
    assert_eq!(sample_times(&d), [rfc3339(NOW - H + 300), rfc3339(NOW + 60), rfc3339(NOW + 14 * 86_400 - 2 * H + 1)]);
}

#[test]
fn a_sample_from_the_future_or_without_a_time_leaves() {
    let d = Dirs::new();
    let file = json!({ "schema_version": 1, "samples": [
        { "at": rfc3339(NOW + 86_400), "mount_point": DATA, "total": 1, "free": 1 },
        { "at": "not a time", "mount_point": DATA, "total": 1, "free": 1 },
        { "at": rfc3339(NOW - 60), "mount_point": DATA, "total": 1, "free": 1 },
    ]});
    fs::write(d.data.join("space.json"), file.to_string()).unwrap();
    assert_eq!(space::read_samples(&d.env()).len(), 3);
    record_at(&d, NOW, 1);
    assert_eq!(sample_times(&d), [rfc3339(NOW - 60), rfc3339(NOW)]);
}

#[test]
fn a_missing_corrupt_or_other_version_samples_file_reads_as_empty() {
    let d = Dirs::new();
    assert!(space::read_samples(&d.env()).is_empty(), "missing");
    let other = json!({ "schema_version": 2, "samples": [{ "at": rfc3339(NOW - 60), "mount_point": DATA, "total": 1, "free": 1 }] });
    for text in ["{ not json".to_owned(), "[]".to_owned(), other.to_string()] {
        fs::write(d.data.join("space.json"), &text).unwrap();
        assert!(space::read_samples(&d.env()).is_empty(), "{text}");
        record_at(&d, NOW, 1);
        assert_eq!(sample_times(&d), [rfc3339(NOW)], "{text}: replaced by a file of this version");
    }
}

#[test]
fn the_check_writes_nothing_and_the_record_only_space_json_atomically() {
    let d = Dirs::new();
    d.save(&scanned(500 * G, 300 * G));
    let saved = fs::read(d.data.join("last-scan.json")).unwrap();
    let r = d.check(&Volume(500 * G, 200 * G), true);
    assert_eq!(d.names(), ["last-scan.json"]);
    let path = space::record(&d.env(), &r).unwrap();
    assert_eq!(path, d.data.join("space.json"));
    assert_eq!(d.names(), ["last-scan.json", "space.json"], "no temporary file is left");
    assert_eq!(fs::read(d.data.join("last-scan.json")).unwrap(), saved);
    // Into a data folder that is not there yet, as a first run.
    let new = d.data.join("new");
    let env = Env::with_paths(&d.home, &new, &new.join("logs"), UNIX_EPOCH + Duration::from_secs(NOW)).unwrap();
    space::record(&env, &r).unwrap();
    let names: Vec<_> = fs::read_dir(&new).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, ["space.json"]);
}

/// The committed schema of `ddc space --json` is the one the types give.
#[test]
#[allow(clippy::disallowed_methods)] // DDC_UPDATE_SCHEMA regenerates the committed file
fn committed_space_schema_is_current() {
    let schema = serde_json::to_string_pretty(&schemars::schema_for!(SpaceReport)).unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/space.schema.json");
    if std::env::var_os("DDC_UPDATE_SCHEMA").is_some() {
        fs::write(&path, &schema).unwrap();
    }
    let committed = fs::read_to_string(&path).unwrap_or_default();
    assert!(committed == schema, "docs/space.schema.json is stale: run DDC_UPDATE_SCHEMA=1 cargo test");
}

/// The app's fixtures (story "Popover: say when used space grew since the
/// last scan") are reports the CLI wrote: the rule decided them, and they
/// are valid against the committed schema. `DDC_UPDATE_SCHEMA` writes them.
#[test]
#[allow(clippy::disallowed_methods)] // DDC_UPDATE_SCHEMA regenerates the committed files
fn the_apps_space_fixtures_are_reports_the_cli_wrote() {
    let at = |s: &str| s.to_owned();
    let saved = |source, when: &str, total, free| Saved {
        baseline: Baseline { source, at: at(when), free_bytes: free },
        total_bytes: total,
    };
    let checked = "2026-11-03T10:00:00Z";
    let fixtures = [
        // 198 GiB more in use than at a scan 26 hours before.
        (
            "space-grew.json",
            space::decide(
                at(checked),
                volume(500 * G, 100 * G),
                Some(saved(BaselineSource::Scan, "2026-11-02T08:00:00Z", 500 * G, 298 * G)),
                true,
            ),
        ),
        // 92 % full, and 30 GiB more in use than after a clean.
        (
            "space-low-disk.json",
            space::decide(
                at(checked),
                volume(500 * G, 40 * G),
                Some(saved(BaselineSource::Clean, "2026-11-03T07:15:00Z", 500 * G, 70 * G)),
                true,
            ),
        ),
        // No saved scan yet.
        ("space-quiet.json", space::decide(at(checked), volume(500 * G, 200 * G), None, true)),
    ];
    let states: Vec<SpaceState> = fixtures.iter().map(|(_, r)| r.state).collect();
    assert_eq!(states, [SpaceState::Grew, SpaceState::LowDisk, SpaceState::Quiet]);
    assert!(fixtures[1].1.grew && fixtures[2].1.baseline.is_none());
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/Packages/DDCKit/Tests/DDCKitTests/Fixtures");
    let schema: Value = serde_json::from_str(
        &fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/space.schema.json")).unwrap(),
    )
    .unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    for (name, report) in &fixtures {
        let text = serde_json::to_string_pretty(report).unwrap() + "\n";
        if std::env::var_os("DDC_UPDATE_SCHEMA").is_some() {
            fs::write(dir.join(name), &text).unwrap();
        }
        let committed = fs::read_to_string(dir.join(name)).unwrap_or_default();
        assert!(committed == text, "{name} is stale: run DDC_UPDATE_SCHEMA=1 cargo test");
        let value: Value = serde_json::from_str(&committed).unwrap();
        let errors: Vec<String> = validator.iter_errors(&value).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "{name}: {errors:?}");
    }
}
