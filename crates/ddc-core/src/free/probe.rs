//! The location probe, the guard of an area action (safety ADR, amendment
//! of 2026-10-28). It runs immediately before the command, and again in the
//! dry run. Folders are compared by `(dev, ino)`: the disk report spells an
//! area through `/System/Volumes/Data`, the tools through the firmlink.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::model::Refusal;
use crate::brew::Budget;
use crate::conda::{self, same_folder};
use crate::disk::Sources;
use crate::disk::simulators::{RawDevice, RawRuntime};
use crate::exec::{CommandRunner, ProcessTable, guard};
use crate::plan::normalised;
use crate::platform::fsops::can_write;

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
    /// As `brew --prefix` printed it.
    pub prefix: PathBuf,
    pub cellar: PathBuf,
    pub cache: PathBuf,
}

/// `brew` is the area's own; `--prefix` is the area's folder, `--cellar`
/// inside it, `--cache` the `cache.homebrew` rule's folder. Each query
/// has what is left of `budget`, at most `TOOL_TIMEOUT`.
pub(crate) fn brew(
    commands: &dyn CommandRunner,
    sources: &Sources,
    home: &Path,
    env: &[(&str, &str)],
    budget: &Budget,
) -> Result<BrewTarget, Refusal> {
    let exe = commands.at(&sources.brew).ok_or(Refusal::BrewMissing)?;
    let ask = |flag: &str| -> Result<PathBuf, Refusal> {
        let out = commands
            .run(&exe, &[flag.to_owned()], env, budget.remaining().min(crate::disk::simulators::TOOL_TIMEOUT))
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
    Ok(BrewTarget { exe, prefix, cellar, cache: ours })
}

/// The conda of an `/opt` install, as the probe found it, and the folder
/// its command is given.
pub(crate) struct CondaTarget {
    /// The install's own `bin/conda`.
    pub exe: PathBuf,
    /// Built from conda's `root_prefix`, so spelled as conda spells it
    /// (`/opt/miniconda3/envs/x`, never through `/System/Volumes/Data`).
    pub path: PathBuf,
    /// [`Self::path`] as conda is given it.
    pub spelled: String,
}

/// The `/opt` install whose folder is `prefix`, by device and inode.
fn opt_install(sources: &Sources, prefix: &Path) -> Result<PathBuf, Refusal> {
    conda::PREFIXES.iter().map(|p| sources.opt.join(p)).find(|p| same_folder(p, prefix)).ok_or(Refusal::CondaNotInOpt)
}

/// `path` lies in the home folder, by its spelling, through a link to the
/// home folder, or once every link in it is resolved (`/opt/miniconda3`
/// linked to `~/miniconda3`).
fn in_home(path: &Path, home: &Path) -> bool {
    if path.starts_with(home) || path.ancestors().any(|a| same_folder(a, home)) {
        return true;
    }
    match (path.canonicalize(), home.canonicalize()) {
        (Ok(path), Ok(home)) => path.starts_with(home),
        _ => false,
    }
}

/// The install answered, and its `root_prefix` is its own folder.
fn asked(install: Option<&conda::Install>, prefix: &Path) -> Result<conda::Install, Refusal> {
    let install = install.ok_or(Refusal::CondaMissing)?;
    if !same_folder(&install.settings.root_prefix, prefix) {
        return Err(Refusal::CondaRootMismatch);
    }
    Ok(install.clone())
}

fn target(install: &conda::Install, path: PathBuf) -> Result<CondaTarget, Refusal> {
    let spelled = conda::spelled(&path).ok_or(Refusal::CondaNotInOpt)?.to_owned();
    Ok(CondaTarget { exe: install.exe.clone(), path, spelled })
}

/// An environment of an `/opt` install that `conda env remove -p` may
/// take: `<prefix>/envs/<name>` with `conda-meta/history`, not in the home
/// folder, not the base, its parent in `envs_dirs`, writable, used by no
/// running process. What the file system tells is checked before conda
/// starts.
pub(crate) fn conda_environment(
    commands: &dyn CommandRunner,
    sources: &Sources,
    home: &Path,
    procs: &dyn ProcessTable,
    identifier: &Path,
) -> Result<CondaTarget, Refusal> {
    if !identifier.is_absolute() || !normalised(identifier) {
        return Err(Refusal::CondaNotInOpt);
    }
    if in_home(identifier, home) {
        return Err(Refusal::CondaInHome);
    }
    let (Some(envs), Some(name)) = (identifier.parent(), identifier.file_name()) else {
        return Err(Refusal::CondaNotInOpt);
    };
    let prefix = envs.parent().filter(|_| envs.file_name().is_some_and(|n| n == "envs"));
    let prefix = opt_install(sources, prefix.ok_or(Refusal::CondaNotInOpt)?)?;
    if same_folder(identifier, &prefix) {
        return Err(Refusal::CondaBase);
    }
    // A link in `envs` is no environment of this install, whatever it points to.
    if identifier.symlink_metadata().is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(Refusal::CondaNotInOpt);
    }
    if !conda::is_environment(identifier) {
        return Err(Refusal::CondaNotAnEnvironment);
    }
    if !(can_write(identifier) && can_write(envs)) {
        return Err(Refusal::CondaNotWritable);
    }
    let firmlinked = prefix.join("envs").join(name);
    let spellings = [identifier.to_path_buf(), firmlinked, identifier.canonicalize().unwrap_or_default()];
    let absolute: Vec<&Path> = spellings.iter().map(PathBuf::as_path).filter(|p| p.is_absolute()).collect();
    if guard::user_of(&absolute, procs).is_some() {
        return Err(Refusal::CondaInUse);
    }
    let install = asked(conda::ask(commands, &prefix).as_ref(), &prefix)?;
    let path = install.settings.root_prefix.join("envs").join(name);
    if install.is_root(&path) {
        return Err(Refusal::CondaBase);
    }
    if !install.claims_environment(&path) {
        return Err(Refusal::CondaNotInEnvsDirs);
    }
    target(&install, path)
}

/// The package folder of the `/opt` install at `identifier` that `conda
/// clean` may take: writable (checked before conda starts), in its
/// `pkgs_dirs`, no install that lists it linking by symlink, and the
/// clean limited to it by `CONDA_PKGS_DIRS`. Every known install is asked,
/// the home folder's too: `~/.conda/pkgs` is in every list.
pub(crate) fn conda_packages(
    commands: &dyn CommandRunner,
    sources: &Sources,
    home: &Path,
    identifier: &Path,
) -> Result<CondaTarget, Refusal> {
    if !identifier.is_absolute() || !normalised(identifier) {
        return Err(Refusal::CondaNotInOpt);
    }
    let prefix = opt_install(sources, identifier)?;
    let pkgs = prefix.join("pkgs");
    if in_home(&pkgs, home) {
        return Err(Refusal::CondaInHome);
    }
    if !pkgs.is_dir() {
        return Err(Refusal::CondaPackagesNotListed);
    }
    if !can_write(&pkgs) {
        return Err(Refusal::CondaNotWritable);
    }
    let installs = conda::Installs::new(commands, conda::candidates_in(home, &sources.opt));
    let all = installs.get();
    let install = asked(all.iter().find(|i| i.prefix == prefix), &prefix)?;
    let path = install.settings.root_prefix.join("pkgs");
    if !install.lists_package_folder(&path) {
        return Err(Refusal::CondaPackagesNotListed);
    }
    if all.iter().any(|i| i.lists_package_folder(&path) && i.softlinks()) {
        return Err(Refusal::CondaSoftlinks);
    }
    let target = target(&install, path)?;
    if !install.scoped_to(commands, &target.path, &target.spelled) {
        return Err(Refusal::CondaNotScoped);
    }
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_whole_components_below_the_root_are_under_it() {
        // macOS paths, which are not absolute on Windows: until story "Windows
        // scanner and rule catalog" adds Windows-shaped inputs.
        if crate::platform::skipped_without(
            "only_whole_components_below_the_root_are_under_it",
            crate::platform::testing::posix_paths(),
        )
        .is_none()
        {
            return;
        }
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
        // macOS paths, which are not absolute on Windows: until story "Windows
        // scanner and rule catalog" adds Windows-shaped inputs.
        if crate::platform::skipped_without(
            "the_asset_folder_is_the_nearest_dot_asset_ancestor_under_assets",
            crate::platform::testing::posix_paths(),
        )
        .is_none()
        {
            return;
        }
        let root = Path::new("/S/AssetsV2");
        assert_eq!(
            asset_folder(Path::new("/S/AssetsV2/com_x/abc.asset/AssetData"), root),
            Some(PathBuf::from("/S/AssetsV2/com_x/abc.asset"))
        );
        assert_eq!(asset_folder(Path::new("/S/AssetsV2/com_x/AssetData"), root), None);
        assert_eq!(asset_folder(Path::new("/S/x.asset/AssetData"), root), None);
    }
}
