//! The volume of each scan root (ADR "Menu bar app: Swift shell over the ddc
//! CLI", §6 item 9). `statfs` is a port, so a test can put roots on two volumes.

use std::io;
use std::path::Path;

use crate::model::Volume;
/// What a [`StatFs`] answers: a fake names it here, not under `platform`.
pub use crate::platform::statfs::VolumeStats;

pub trait StatFs {
    fn statfs(&self, path: &Path) -> io::Result<VolumeStats>;
    /// Every mounted file system.
    fn mounts(&self) -> io::Result<Vec<VolumeStats>> {
        Err(io::ErrorKind::Unsupported.into())
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
            }),
        }
    }
    out
}
