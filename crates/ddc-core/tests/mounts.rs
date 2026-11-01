//! Each filesystem and root-only file of the Gate's linux-mounts job is what it
//! claims to be (story "Linux CI mounts: a filesystem matrix job with root-only
//! steps", DDC-010). The stories that test the walk and the clean on them rely
//! on that. Elsewhere every test here skips, in the environment group of
//! `.config/native-skips/linux.txt`.
#![cfg(target_os = "linux")]

mod common;

use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

use common::{Mount, Prepared, TreeBuilder};
use rustix::fs::IFlags;

/// `FS_CASEFOLD_FL`, which `rustix` does not name.
const CASEFOLD: u32 = 0x4000_0000;

fn flags(path: &std::path::Path) -> IFlags {
    let f = fs::File::open(path).unwrap_or_else(|e| panic!("open {path:?}: {e}"));
    rustix::fs::ioctl_getflags(&f).unwrap_or_else(|e| panic!("FS_IOC_GETFLAGS {path:?}: {e}"))
}

fn me() -> u32 {
    rustix::process::getuid().as_raw()
}

#[test]
fn a_btrfs_mount_is_btrfs_in_mountinfo() {
    let Some(dir) = common::mount("a_btrfs_mount_is_btrfs_in_mountinfo", Mount::Btrfs) else { return };
    let vol = ddc_core::platform::statfs::statfs(&dir).unwrap();
    assert_eq!(vol.fs_type, "btrfs");
    assert_eq!(vol.mount_point, dir);
}

#[test]
fn a_btrfs_subvolume_is_a_root_inode_with_a_device_of_its_own() {
    let Some(top) = common::mount("a_btrfs_subvolume_is_a_root_inode_with_a_device_of_its_own", Mount::Btrfs) else {
        return;
    };
    let Some(sub) = common::mount("a_btrfs_subvolume_is_a_root_inode_with_a_device_of_its_own", Mount::BtrfsSubvolume)
    else {
        return;
    };
    let md = fs::metadata(&sub).unwrap();
    assert_eq!(md.ino(), 256, "a subvolume's root is inode 256");
    assert_ne!(md.dev(), fs::metadata(&top).unwrap().dev());
    // Reached by path, not mounted: mountinfo puts it on the top's mount.
    assert_eq!(ddc_core::platform::statfs::statfs(&sub).unwrap().mount_point, top);
}

#[test]
fn a_read_only_btrfs_snapshot_refuses_a_new_file() {
    let Some(snap) = common::mount("a_read_only_btrfs_snapshot_refuses_a_new_file", Mount::BtrfsSnapshotRo) else {
        return;
    };
    assert!(snap.join("file").is_file(), "the snapshot holds the subvolume's file");
    let err = fs::write(snap.join("new"), b"x").unwrap_err();
    assert_eq!(err.raw_os_error(), Some(rustix::io::Errno::ROFS.raw_os_error()), "{err}");
}

#[test]
fn a_writable_btrfs_snapshot_takes_a_new_file() {
    let Some(snap) = common::mount("a_writable_btrfs_snapshot_takes_a_new_file", Mount::BtrfsSnapshotRw) else {
        return;
    };
    assert!(snap.join("file").is_file(), "the snapshot holds the subvolume's file");
    let tree = TreeBuilder::new_in(&snap);
    let file = tree.file("new", 10);
    assert_eq!(common::id(&file).0, common::id(&snap).0, "the new file is on the snapshot's device");
}

#[test]
fn an_xfs_mount_is_xfs_in_mountinfo() {
    let Some(dir) = common::mount("an_xfs_mount_is_xfs_in_mountinfo", Mount::Xfs) else { return };
    assert_eq!(ddc_core::platform::statfs::statfs(&dir).unwrap().fs_type, "xfs");
}

#[test]
fn a_casefold_folder_finds_a_name_in_another_case() {
    let Some(dir) = common::mount("a_casefold_folder_finds_a_name_in_another_case", Mount::Casefold) else { return };
    assert_ne!(flags(&dir).bits() & CASEFOLD, 0, "the folder carries FS_CASEFOLD_FL");
    let tree = TreeBuilder::new_in(&dir);
    tree.dir("Cache");
    assert!(tree.path("cache").is_dir(), "cache finds Cache");
}

#[test]
fn an_exfat_mount_finds_a_name_in_another_case() {
    let Some(dir) = common::mount("an_exfat_mount_finds_a_name_in_another_case", Mount::Exfat) else { return };
    let tree = TreeBuilder::new_in(&dir);
    tree.dir("Cache");
    assert!(tree.path("cache").is_dir(), "cache finds Cache");
}

#[test]
fn a_tmpfs_mount_is_the_users() {
    let Some(dir) = common::mount("a_tmpfs_mount_is_the_users", Mount::Tmpfs) else { return };
    assert_eq!(fs::metadata(&dir).unwrap().uid(), me());
    assert_eq!(ddc_core::platform::statfs::statfs(&dir).unwrap().mount_point, dir);
}

#[test]
fn a_root_topped_tmpfs_holds_a_folder_of_the_users() {
    let Some(dir) = common::mount("a_root_topped_tmpfs_holds_a_folder_of_the_users", Mount::TmpfsRootTop) else {
        return;
    };
    let top = dir.parent().unwrap();
    assert_eq!(fs::metadata(top).unwrap().uid(), 0, "the top is root's");
    assert_eq!(ddc_core::platform::statfs::statfs(&dir).unwrap().mount_point, top);
    assert_eq!(fs::metadata(&dir).unwrap().uid(), me());
    let err = fs::create_dir(top.join(format!(".Trash-{}", me()))).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::PermissionDenied, "no .Trash-$uid at the top: {err}");
}

#[test]
fn a_zfs_child_dataset_is_a_mount_with_a_snapshot() {
    let Some(pool) = common::mount("a_zfs_child_dataset_is_a_mount_with_a_snapshot", Mount::Zfs) else { return };
    let Some(child) = common::mount("a_zfs_child_dataset_is_a_mount_with_a_snapshot", Mount::ZfsChild) else {
        return;
    };
    let vol = ddc_core::platform::statfs::statfs(&child).unwrap();
    assert_eq!((vol.fs_type.as_str(), vol.mount_point.as_path()), ("zfs", child.as_path()));
    let top = ddc_core::platform::statfs::statfs(&pool).unwrap();
    assert!(vol.mounted_from.starts_with(&format!("{}/", top.mounted_from)), "{vol:?} is a child of {top:?}");
    assert!(child.join(".zfs/snapshot/ddc/file").is_file(), "the snapshot is reachable by path");
}

#[test]
fn an_nfs_mount_is_nfs_and_the_users() {
    let Some(dir) = common::mount("an_nfs_mount_is_nfs_and_the_users", Mount::Nfs) else { return };
    assert!(ddc_core::platform::statfs::statfs(&dir).unwrap().fs_type.starts_with("nfs"));
    let tree = TreeBuilder::new_in(&dir);
    let file = tree.file("f", 10);
    assert_eq!(fs::metadata(file).unwrap().uid(), me());
}

#[test]
fn a_squashed_nfs_mount_gives_a_new_file_to_nobody() {
    let Some(dir) = common::mount("a_squashed_nfs_mount_gives_a_new_file_to_nobody", Mount::NfsSquash) else {
        return;
    };
    let tree = TreeBuilder::new_in(&dir);
    let file = tree.file("f", 10);
    assert_eq!(fs::metadata(file).unwrap().uid(), 65534);
}

#[test]
fn a_cifs_mount_finds_a_name_in_another_case() {
    let Some(dir) = common::mount("a_cifs_mount_finds_a_name_in_another_case", Mount::Cifs) else { return };
    assert_eq!(ddc_core::platform::statfs::statfs(&dir).unwrap().fs_type, "cifs");
    let tree = TreeBuilder::new_in(&dir);
    tree.dir("Cache");
    assert!(tree.path("cache").is_dir(), "cache finds Cache");
}

#[test]
fn a_bind_mount_has_its_parents_device_and_a_mountinfo_entry() {
    let Some(root) = common::mount("a_bind_mount_has_its_parents_device_and_a_mountinfo_entry", Mount::Bind) else {
        return;
    };
    let inner = root.join("inner");
    assert_eq!(common::id(&root).0, common::id(&inner).0, "the same device on both sides");
    assert!(inner.join("file").is_file(), "the bound folder's file shows through");
    assert_eq!(ddc_core::platform::statfs::statfs(&inner).unwrap().mount_point, inner);
}

#[test]
fn the_prepared_device_is_a_character_device() {
    let Some(dev) = common::prepared("the_prepared_device_is_a_character_device", Prepared::Device) else { return };
    assert!(fs::symlink_metadata(dev).unwrap().file_type().is_char_device());
}

#[test]
fn the_prepared_immutable_file_carries_its_flag_and_refuses_a_rename() {
    let Some(file) =
        common::prepared("the_prepared_immutable_file_carries_its_flag_and_refuses_a_rename", Prepared::Immutable)
    else {
        return;
    };
    assert!(flags(&file).contains(IFlags::IMMUTABLE));
    let err = fs::rename(&file, file.with_extension("moved")).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::PermissionDenied, "{err}");
}

#[test]
fn the_prepared_append_only_file_carries_its_flag() {
    let Some(file) = common::prepared("the_prepared_append_only_file_carries_its_flag", Prepared::AppendOnly) else {
        return;
    };
    assert!(flags(&file).contains(IFlags::APPEND));
}
