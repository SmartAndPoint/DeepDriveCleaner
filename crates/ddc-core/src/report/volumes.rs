//! The volume of each scan root (ADR "Menu bar app: Swift shell over the ddc
//! CLI", §6 item 9). `statfs` is a port, so a test can put roots on two volumes.

use std::io;
use std::path::{Path, PathBuf};

use crate::model::{IdMapping, Volume};
/// What a [`StatFs`] answers: a fake names it here, not under `platform`.
pub use crate::platform::statfs::VolumeStats;

pub trait StatFs {
    fn statfs(&self, path: &Path) -> io::Result<VolumeStats>;
    /// Every mounted file system.
    fn mounts(&self) -> io::Result<Vec<VolumeStats>> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// The owner and the mount id of the run's holder record, as the
    /// filesystem shows them (safety ADR, amendment of 2026-11-17, "a
    /// mapped root"). `None`: no root is mapped.
    fn record(&self, _path: &Path) -> Option<(u32, u64)> {
        None
    }
    /// A root's mount id, and whether the user may write into and search it.
    fn root_access(&self, _path: &Path) -> Option<(u64, bool)> {
        None
    }
}

pub struct SystemStatFs;

impl StatFs for SystemStatFs {
    fn statfs(&self, path: &Path) -> io::Result<VolumeStats> {
        crate::platform::statfs::statfs(path)
    }
    fn mounts(&self) -> io::Result<Vec<VolumeStats>> {
        crate::platform::statfs::mounts()
    }
    fn record(&self, path: &Path) -> Option<(u32, u64)> {
        crate::platform::meta::owner_and_mount(path)
    }
    fn root_access(&self, path: &Path) -> Option<(u64, bool)> {
        crate::platform::meta::mount_and_writable(path)
    }
}

/// One entry per mount point, in the order of the roots, each with the roots
/// on it. A root whose volume cannot be read (a share gone during the scan)
/// is left out: the low-disk check cannot fire for it.
pub fn volumes(roots: &[&Path], statfs: &dyn StatFs) -> Vec<Volume> {
    let mut out: Vec<Volume> = Vec::new();
    for root in roots {
        let Ok(st) = statfs.statfs(root) else { continue };
        match out.iter_mut().find(|v| v.mount_point == st.mount_point) {
            Some(v) => v.roots.push(root.to_path_buf()),
            None => out.push(Volume {
                mount_point: st.mount_point,
                roots: vec![root.to_path_buf()],
                free_bytes: st.available_bytes,
                total_bytes: st.total_bytes,
                read_at: None,
                id_mapping: None,
            }),
        }
    }
    out
}

/// The uid the kernel shows for an owner name an NFSv4 client cannot map
/// (its -2): every such owner alike, so it maps to no one.
pub const UNMAPPED_UID: u32 = u32::MAX - 1;

/// The owner M the server shows for the run's own holder record, when it
/// could be a mapping: neither the user, nor root, nor [`UNMAPPED_UID`].
/// The walk counts M's entries ([`crate::scan::WalkOptions::mapped`]).
pub fn mapped_owner(record: Option<(u32, u64)>, me: u32) -> Option<(u32, u64)> {
    record.filter(|&(owner, _)| owner != me && owner != 0 && owner != UNMAPPED_UID)
}

/// Whether a root is mapped to the record's owner (safety ADR, amendment
/// of 2026-11-17, "a mapped root"): its mount is NFS by mountinfo's type,
/// it is the home or inside it, it lies on the record's own mount, and the
/// user may write into and search it. The filesystem names are mountinfo's;
/// macOS calls an NFS mount `nfs` too, but its probe reads no owner
/// ([`StatFs::record`] is `None` there), so only Linux maps.
pub fn is_mapped(fs_type: &str, root: &Path, home: &Path, record_mount: u64, access: Option<(u64, bool)>) -> bool {
    matches!(fs_type, "nfs" | "nfs4")
        && root.starts_with(home)
        && access.is_some_and(|(mount, writable)| mount == record_mount && writable)
}

/// The roots, as the walk will hold them, that are mapped to the
/// record's owner ([`is_mapped`]), decided before the walk so that it and
/// every finding see the same answer.
pub fn mapped_roots(roots: &[PathBuf], statfs: &dyn StatFs, home: &Path, mapped: Option<(u32, u64)>) -> Vec<PathBuf> {
    let Some((_, record_mount)) = mapped else { return Vec::new() };
    roots
        .iter()
        .filter(|r| {
            statfs.statfs(r).is_ok_and(|st| is_mapped(&st.fs_type, r, home, record_mount, statfs.root_access(r)))
        })
        .cloned()
        .collect()
}

/// Sets each volume's [`IdMapping`] from the `mapped` roots on it, with
/// `entries(root)` the walk's count of M's entries under a root.
pub fn id_mappings(
    volumes: &mut [Volume],
    statfs: &dyn StatFs,
    owner: Option<u32>,
    mapped: &[PathBuf],
    entries: &dyn Fn(&Path) -> u64,
) {
    let Some(owner) = owner else { return };
    for v in volumes {
        let roots: Vec<PathBuf> = v.roots.iter().filter(|r| mapped.contains(r)).cloned().collect();
        let Some(fs_type) = roots.first().and_then(|r| statfs.statfs(r).ok()).map(|st| st.fs_type) else {
            continue;
        };
        let entries = roots.iter().map(|r| entries(r)).sum();
        v.id_mapping = Some(IdMapping { owner, roots, entries, filesystem: fs_type });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The holder record's owner is a mapping only when it is someone else
    /// than the user, root, or the kernel's unmapped -2.
    #[test]
    fn only_another_users_record_owner_can_be_a_mapping() {
        let me = 1000;
        assert_eq!(mapped_owner(Some((65534, 7)), me), Some((65534, 7)));
        for owner in [me, 0, UNMAPPED_UID] {
            assert_eq!(mapped_owner(Some((owner, 7)), me), None, "{owner}");
        }
        assert_eq!(mapped_owner(None, me), None, "no record, or one that cannot be read");
    }

    /// A root is mapped on NFS only, in the home, on the record's own mount,
    /// writable and searchable; every condition that fails says no.
    #[test]
    fn a_root_is_mapped_only_when_every_condition_holds() {
        let (home, root) = (Path::new("/h"), Path::new("/h/work"));
        let ok = Some((7, true));
        assert!(is_mapped("nfs4", root, home, 7, ok));
        assert!(is_mapped("nfs", home, home, 7, ok));
        for fs in ["cifs", "smb3", "fuse.sshfs", "fuse.gocryptfs", "ext4", ""] {
            assert!(!is_mapped(fs, root, home, 7, ok), "{fs}");
        }
        assert!(!is_mapped("nfs4", Path::new("/srv/shared"), home, 7, ok), "outside the home");
        assert!(!is_mapped("nfs4", root, home, 8, ok), "the record on another mount");
        assert!(!is_mapped("nfs4", root, home, 7, Some((7, false))), "not writable");
        assert!(!is_mapped("nfs4", root, home, 7, None), "unreadable");
    }
}
