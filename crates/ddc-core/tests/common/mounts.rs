//! The filesystems of the Gate's linux-mounts job (story "Linux CI mounts: a
//! filesystem matrix job with root-only steps", DDC-010). `scripts/ci-mounts.sh`
//! makes them and exports where each one is; a test asks for one with
//! `let Some(dir) = common::mount("name", Mount::Btrfs) else { return };`.
//! Elsewhere the variable is unset, and the test skips with a reason that
//! starts "no <kind> mount": `scripts/native-skips.sh` takes such a skip only
//! from a test of the environment group of `.config/native-skips/linux.txt`.
//! A variable that is set but names no such filesystem fails the test: a broken
//! job must never pass on the runner's own disk.

use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mount {
    Btrfs,
    /// A subvolume the user made, at the top of [`Mount::Btrfs`].
    BtrfsSubvolume,
    BtrfsSnapshotRo,
    BtrfsSnapshotRw,
    Xfs,
    /// A folder with ext4's casefold flag (`chattr +F`).
    Casefold,
    Exfat,
    /// The user's own tmpfs.
    Tmpfs,
    /// The user's folder in a tmpfs whose top is root's.
    TmpfsRootTop,
    Zfs,
    /// A child dataset of [`Mount::Zfs`], with a snapshot `ddc`.
    ZfsChild,
    Nfs,
    /// An NFS export with `all_squash`: what the user writes belongs to nobody.
    NfsSquash,
    Cifs,
    /// A test root on the runner's disk, with a bind mount of that disk at
    /// `inner`.
    Bind,
}

impl Mount {
    pub fn var(self) -> &'static str {
        match self {
            Mount::Btrfs => "DDC_MOUNT_BTRFS",
            Mount::BtrfsSubvolume => "DDC_MOUNT_BTRFS_SUBVOLUME",
            Mount::BtrfsSnapshotRo => "DDC_MOUNT_BTRFS_SNAPSHOT_RO",
            Mount::BtrfsSnapshotRw => "DDC_MOUNT_BTRFS_SNAPSHOT_RW",
            Mount::Xfs => "DDC_MOUNT_XFS",
            Mount::Casefold => "DDC_MOUNT_CASEFOLD",
            Mount::Exfat => "DDC_MOUNT_EXFAT",
            Mount::Tmpfs => "DDC_MOUNT_TMPFS",
            Mount::TmpfsRootTop => "DDC_MOUNT_TMPFS_ROOT_TOP",
            Mount::Zfs => "DDC_MOUNT_ZFS",
            Mount::ZfsChild => "DDC_MOUNT_ZFS_CHILD",
            Mount::Nfs => "DDC_MOUNT_NFS",
            Mount::NfsSquash => "DDC_MOUNT_NFS_SQUASH",
            Mount::Cifs => "DDC_MOUNT_CIFS",
            Mount::Bind => "DDC_MOUNT_BIND",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mount::Btrfs => "btrfs",
            Mount::BtrfsSubvolume => "btrfs subvolume",
            Mount::BtrfsSnapshotRo => "btrfs read-only snapshot",
            Mount::BtrfsSnapshotRw => "btrfs writable snapshot",
            Mount::Xfs => "xfs",
            Mount::Casefold => "casefold ext4",
            Mount::Exfat => "exfat",
            Mount::Tmpfs => "tmpfs",
            Mount::TmpfsRootTop => "root-topped tmpfs",
            Mount::Zfs => "zfs",
            Mount::ZfsChild => "zfs child dataset",
            Mount::Nfs => "nfs",
            Mount::NfsSquash => "squashed nfs",
            Mount::Cifs => "cifs",
            Mount::Bind => "bind",
        }
    }

    /// `statfs`'s `f_type` of the filesystem (`linux/magic.h`). The casefold
    /// folder and the bind mount are ext4: their tests prove the rest.
    pub fn magic(self) -> u32 {
        match self {
            Mount::Btrfs | Mount::BtrfsSubvolume | Mount::BtrfsSnapshotRo | Mount::BtrfsSnapshotRw => 0x9123_683E,
            Mount::Xfs => 0x5846_5342,
            Mount::Casefold | Mount::Bind => 0xEF53,
            Mount::Exfat => 0x2011_BAB0,
            Mount::Tmpfs | Mount::TmpfsRootTop => 0x0102_1994,
            Mount::Zfs | Mount::ZfsChild => 0x2FC1_2FC1,
            Mount::Nfs | Mount::NfsSquash => 0x6969,
            Mount::Cifs => 0xFE53_4D42,
        }
    }
}

/// `statfs`'s `f_type` of `path`, as the 32-bit magic number it is.
pub fn magic_of(path: &std::path::Path) -> u32 {
    let st = rustix::fs::statfs(path).unwrap_or_else(|e| panic!("statfs {path:?}: {e}"));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a magic number fits 32 bits
    let magic = st.f_type as u32;
    magic
}

/// Where the job made `kind`, or `None` after printing why the test skips.
pub fn mount(test: &str, kind: Mount) -> Option<PathBuf> {
    let dir = given(test, kind.var(), &format!("no {} mount", kind.name()))?;
    let magic = magic_of(&dir);
    assert_eq!(
        magic,
        kind.magic(),
        "{test}: {} = {dir:?} is not a {} mount (f_type {magic:#x})",
        kind.var(),
        kind.name()
    );
    Some(dir)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prepared {
    /// A character device (`/dev/null`'s numbers).
    Device,
    /// A file with `chattr +i`.
    Immutable,
    /// A file with `chattr +a`.
    AppendOnly,
}

impl Prepared {
    pub fn var(self) -> &'static str {
        match self {
            Prepared::Device => "DDC_FIXTURE_DEVICE",
            Prepared::Immutable => "DDC_FIXTURE_IMMUTABLE",
            Prepared::AppendOnly => "DDC_FIXTURE_APPEND_ONLY",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Prepared::Device => "device",
            Prepared::Immutable => "immutable",
            Prepared::AppendOnly => "append-only",
        }
    }
}

/// A file only root can make, which the job prepared, each in a folder of
/// its own; `None` after printing why the test skips.
pub fn prepared(test: &str, what: Prepared) -> Option<PathBuf> {
    given(test, what.var(), &format!("no {} fixture", what.name()))
}

#[allow(clippy::disallowed_methods)] // a switch of the test harness, not of the scanner
fn given(test: &str, var: &str, missing: &str) -> Option<PathBuf> {
    match std::env::var_os(var).filter(|v| !v.is_empty()) {
        None => {
            eprintln!("skipped {test}: {missing} ({var} unset)");
            None
        }
        Some(v) => {
            let path = PathBuf::from(v);
            assert!(std::fs::symlink_metadata(&path).is_ok(), "{test}: {var} = {path:?} does not exist");
            Some(path)
        }
    }
}
