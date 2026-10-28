//! The location probe, the guard of an area action (safety ADR, amendment
//! of 2026-10-28). It runs immediately before the command, and again in the
//! dry run. Folders are compared by `(dev, ino)`: the disk report spells an
//! area through `/System/Volumes/Data`, the tools through the firmlink.

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use super::model::Refusal;
use crate::disk::Sources;
use crate::disk::simulators::{RawDevice, RawRuntime};
use crate::exec::CommandRunner;
use crate::plan::normalised;

/// Exactly one runtime has `identifier`, and it may be deleted: under
/// AssetsV2, deletable, not deleting, joinable to its devices, none booted.
/// `devices` is `None` when `list devices -j` failed. The runtime is
/// matched byte for byte in a fresh `runtime list -j`; the result is the
/// `<hash>.asset` folder it frees.
pub(crate) fn runtime(
    runtimes: &[RawRuntime],
    devices: Option<&BTreeMap<String, Vec<RawDevice>>>,
    identifier: &str,
    sources: &Sources,
) -> Result<PathBuf, Refusal> {
    let mut matching = runtimes.iter().filter(|r| r.identifier == identifier);
    let (Some(runtime), None) = (matching.next(), matching.next()) else { return Err(Refusal::UnknownRuntime) };
    if !runtime.parent_mount_path.as_deref().is_some_and(|p| strictly_under(p, &sources.assets)) {
        return Err(Refusal::NotUnderAssets);
    }
    if runtime.deletable == Some(false) {
        return Err(Refusal::NotDeletable);
    }
    if runtime.state.as_deref() == Some("Deleting") {
        return Err(Refusal::Deleting);
    }
    let Some(runtime_id) = &runtime.runtime_identifier else { return Err(Refusal::NoRuntimeIdentifier) };
    let asset = runtime.path.as_deref().and_then(|p| asset_folder(p, &sources.assets)).ok_or(Refusal::NoAssetFolder)?;
    let devices = devices.ok_or(Refusal::DevicesUnknown)?;
    // Booting and Shutting Down too: only a device at rest is left alone by the delete.
    if devices.get(runtime_id).is_some_and(|ds| ds.iter().any(|d| d.state != "Shutdown")) {
        return Err(Refusal::Booted);
    }
    Ok(asset)
}

/// The nearest folder holding `path` (or `path` itself) whose name ends in
/// `.asset`, strictly under `assets`.
fn asset_folder(path: &Path, assets: &Path) -> Option<PathBuf> {
    if !strictly_under(path, assets) {
        return None;
    }
    path.ancestors()
        .take_while(|p| strictly_under(p, assets))
        .find(|p| p.file_name().is_some_and(|n| n.to_string_lossy().ends_with(".asset")))
        .map(Path::to_path_buf)
}

/// `path` is absolute, has no `.` or `..`, and lies below `root` by whole
/// components: `AssetsV2Evil` is not under `AssetsV2`.
fn strictly_under(path: &Path, root: &Path) -> bool {
    normalised(path) && path != root && path.starts_with(root)
}

/// The Homebrew of the area, as the probe found it.
pub(crate) struct BrewTarget {
    pub exe: PathBuf,
    pub cellar: PathBuf,
    pub cache: PathBuf,
}

/// `brew` is the area's own; `--prefix` is the area's folder, `--cellar`
/// inside it, `--cache` the `cache.homebrew` rule's folder.
pub(crate) fn brew(
    commands: &dyn CommandRunner,
    sources: &Sources,
    home: &Path,
    env: &[(&str, &str)],
) -> Result<BrewTarget, Refusal> {
    let exe = commands.at(&sources.brew).ok_or(Refusal::BrewMissing)?;
    let ask = |flag: &str| -> Result<PathBuf, Refusal> {
        let out = commands
            .run(&exe, &[flag.to_owned()], env, crate::disk::simulators::TOOL_TIMEOUT)
            .ok()
            .filter(|o| o.success)
            .ok_or(Refusal::BrewMissing)?;
        let path = PathBuf::from(out.stdout.trim());
        if path.is_absolute() { Ok(path) } else { Err(Refusal::BrewMissing) }
    };
    let prefix = ask("--prefix")?;
    if !same_folder(&prefix, &sources.data_root.join("opt/homebrew")) {
        return Err(Refusal::PrefixMismatch);
    }
    let cellar = ask("--cellar")?;
    let inside = match (cellar.canonicalize(), prefix.canonicalize()) {
        (Ok(c), Ok(p)) => c != p && c.starts_with(&p),
        _ => false,
    };
    if !inside {
        return Err(Refusal::CellarOutside);
    }
    let cache = ask("--cache")?;
    let ours = home.join("Library/Caches/Homebrew");
    // Right after a clean removed it, the folder is missing: both lie under
    // the home folder, with no firmlink between them, so paths compare.
    let same = if std::fs::symlink_metadata(&ours).is_ok() {
        same_folder(&cache, &ours)
    } else {
        normalised(&cache) && cache == ours
    };
    if !same {
        return Err(Refusal::CacheMismatch);
    }
    Ok(BrewTarget { exe, cellar, cache: ours })
}

/// Both are folders and the same one, by `(dev, ino)`.
fn same_folder(a: &Path, b: &Path) -> bool {
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.is_dir() && b.is_dir() && a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_whole_components_below_the_root_are_under_it() {
        let root = Path::new("/System/Library/AssetsV2");
        assert!(strictly_under(Path::new("/System/Library/AssetsV2/x/a.asset"), root));
        assert!(!strictly_under(root, root));
        assert!(!strictly_under(Path::new("/System/Library/AssetsV2Evil/a.asset"), root));
        assert!(!strictly_under(Path::new("/System/Library/AssetsV2/../../../etc"), root));
        assert!(!strictly_under(Path::new("/System/Library/AssetsV2/./a.asset"), root));
        assert!(!strictly_under(Path::new("System/Library/AssetsV2/a.asset"), root));
    }

    #[test]
    fn the_asset_folder_is_the_nearest_dot_asset_ancestor_under_assets() {
        let root = Path::new("/S/AssetsV2");
        assert_eq!(
            asset_folder(Path::new("/S/AssetsV2/com_x/abc.asset/AssetData"), root),
            Some(PathBuf::from("/S/AssetsV2/com_x/abc.asset"))
        );
        assert_eq!(asset_folder(Path::new("/S/AssetsV2/com_x/AssetData"), root), None);
        assert_eq!(asset_folder(Path::new("/S/x.asset/AssetData"), root), None);
    }
}
