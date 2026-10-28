//! The startup container and its volumes: `diskutil apfs list -plist`, else
//! `statfs` per mounted volume.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::model::{Container, ContainerSource, VolumeRow};
use super::plist::{self, Value};
use crate::exec::CommandRunner;
use crate::report::volumes::StatFs;
use crate::sys::statfs::VolumeStats;

const DISKUTIL_TIMEOUT: Duration = Duration::from_secs(30);

/// The container holding the volume mounted at `data_root`. The plist also
/// lists the Recovery container and the disk images of simulator runtimes,
/// which are not part of it.
pub(super) fn container(
    commands: &dyn CommandRunner,
    diskutil: &Path,
    statfs: &dyn StatFs,
    data_root: &Path,
) -> Option<Container> {
    let data = statfs.statfs(data_root).ok()?;
    let data_device = device(&data.mounted_from)?;
    let mounts = statfs.mounts().unwrap_or_default();
    let mounted: HashMap<String, PathBuf> =
        mounts.iter().filter_map(|m| Some((device(&m.mounted_from)?, m.mount_point.clone()))).collect();
    from_diskutil(commands, diskutil, &data_device, &mounted)
        .or_else(|| Some(from_statfs(&data, &data_device, &mounts)))
}

/// `disk3s5` from `/dev/disk3s5`, and the volume of a snapshot mount from
/// `/dev/disk3s1s1` (the sealed system is mounted from a snapshot).
fn device(mounted_from: &str) -> Option<String> {
    let name = mounted_from.strip_prefix("/dev/")?;
    let rest = name.strip_prefix("disk")?;
    let parts: Vec<&str> = rest.split('s').collect();
    if parts.len() < 2 || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    Some(format!("disk{}s{}", parts[0], parts[1]))
}

fn from_diskutil(
    commands: &dyn CommandRunner,
    diskutil: &Path,
    data_device: &str,
    mounted: &HashMap<String, PathBuf>,
) -> Option<Container> {
    let exe = commands.at(diskutil)?;
    let args = ["apfs", "list", "-plist"].map(String::from);
    let out = commands.run(&exe, &args, &[], DISKUTIL_TIMEOUT).ok().filter(|o| o.success)?;
    let root = plist::parse(&out.stdout).ok()?;
    let container = root.get("Containers")?.as_array().iter().find(|c| {
        c.get("Volumes").is_some_and(|vs| {
            vs.as_array().iter().any(|v| v.get("DeviceIdentifier").and_then(Value::as_str) == Some(data_device))
        })
    })?;
    let capacity = container.get("CapacityCeiling").and_then(Value::as_u64)?;
    let free = container.get("CapacityFree").and_then(Value::as_u64)?;
    let volumes = container
        .get("Volumes")?
        .as_array()
        .iter()
        .map(|v| {
            let device = v.get("DeviceIdentifier").and_then(Value::as_str).map(str::to_owned);
            VolumeRow {
                name: v.get("Name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                role: v
                    .get("Roles")
                    .and_then(|r| r.as_array().first())
                    .and_then(Value::as_str)
                    .map_or_else(|| "none".to_owned(), str::to_lowercase),
                used_bytes: v.get("CapacityInUse").and_then(Value::as_u64),
                mount_point: device.as_ref().and_then(|d| mounted.get(d)).cloned(),
                device,
            }
        })
        .collect();
    Some(Container {
        source: ContainerSource::Diskutil,
        reference: container.get("ContainerReference").and_then(Value::as_str).map(str::to_owned),
        capacity_bytes: capacity,
        in_use_bytes: capacity.saturating_sub(free),
        volumes,
    })
}

/// The mounted APFS volumes of the Data volume's container (`disk3s…`). On
/// APFS a volume's `f_blocks - f_bfree` is what it uses. Recovery is not
/// mounted, so it is listed as not measured.
fn from_statfs(data: &VolumeStats, data_device: &str, mounts: &[VolumeStats]) -> Container {
    let prefix = data_device.rsplit_once('s').map_or(data_device, |(c, _)| c);
    let mut volumes: Vec<VolumeRow> = Vec::new();
    let mut seen = Vec::new();
    for m in mounts.iter().filter(|m| m.fs_type == "apfs") {
        let Some(dev) = device(&m.mounted_from) else { continue };
        if dev.rsplit_once('s').map(|(c, _)| c) != Some(prefix) || seen.contains(&dev) {
            continue;
        }
        seen.push(dev.clone());
        let role = match m.mount_point.to_str() {
            Some("/") => "system".to_owned(),
            _ => m.mount_point.file_name().map_or_else(|| "none".to_owned(), |n| n.to_string_lossy().to_lowercase()),
        };
        volumes.push(VolumeRow {
            name: m.mount_point.file_name().map_or_else(|| "Macintosh HD".to_owned(), |n| n.to_string_lossy().into()),
            role,
            device: Some(dev),
            used_bytes: Some(m.total_bytes.saturating_sub(m.free_bytes)),
            mount_point: Some(m.mount_point.clone()),
        });
    }
    if !volumes.iter().any(|v| v.device.as_deref() == Some(data_device)) {
        volumes.push(VolumeRow {
            name: "Data".to_owned(),
            role: "data".to_owned(),
            device: Some(data_device.to_owned()),
            used_bytes: Some(data.total_bytes.saturating_sub(data.free_bytes)),
            mount_point: Some(data.mount_point.clone()),
        });
    }
    volumes.push(VolumeRow {
        name: "Recovery".to_owned(),
        role: "recovery".to_owned(),
        device: None,
        used_bytes: None,
        mount_point: None,
    });
    let in_use: u64 = volumes.iter().filter_map(|v| v.used_bytes).sum();
    Container {
        source: ContainerSource::Statfs,
        reference: None,
        capacity_bytes: in_use + data.free_bytes,
        in_use_bytes: in_use,
        volumes,
    }
}

#[cfg(test)]
mod tests {
    use super::device;

    #[test]
    fn devices_drop_the_snapshot() {
        assert_eq!(device("/dev/disk3s5").as_deref(), Some("disk3s5"));
        assert_eq!(device("/dev/disk3s1s1").as_deref(), Some("disk3s1"));
        assert_eq!(device("/dev/disk4"), None);
        assert_eq!(device("map auto_home"), None);
        assert_eq!(device("devfs"), None);
    }
}
