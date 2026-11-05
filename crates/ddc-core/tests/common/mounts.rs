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
    /// A btrfs scan root: subvolume `work` (with `app/node_modules` and the
    /// nested subvolume `nested`), `whole/node_modules` a subvolume,
    /// `holds/node_modules/inner` a subvolume, the snapshots `snap-ro`,
    /// `snap-rw` and `recv/snap-ro` (received), and another btrfs at `data`.
    BtrfsTree,
    /// The subvolume `home` of a btrfs mounted again after a scan of it, so
    /// `home` has another anonymous device: projects `p` and `q`, whose
    /// `node_modules` is kept, and beside `home` the folder `state`, which
    /// holds the scan's data folder `data` and the device `home` had,
    /// `before-dev`.
    BtrfsRemount,
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
    /// A ZFS scan root: child dataset `work` (with `app/node_modules` and
    /// the grandchild `nested`), `whole/node_modules` a dataset,
    /// `holds/node_modules/inner` a dataset, `snapdir` with `snapdir=visible`
    /// and a snapshot `ddc`, `bound` a bind mount of `work`, and the child
    /// `away` mounted at `moved` by a `mountpoint=` of its own.
    ZfsTree,
    /// A ZFS home for the delete test, with the child dataset `work`.
    ZfsDelete,
    /// A ZFS home for the guard test, whose `p/node_modules` is a dataset.
    ZfsGuard,
    Nfs,
    /// An NFS export with `all_squash`: what the user writes belongs to nobody.
    NfsSquash,
    Cifs,
    /// A test root on the runner's disk, with a bind mount of that disk at
    /// `inner`.
    Bind,
    /// The trigger of an autofs direct map, which binds a folder of the
    /// runner's disk when opened. `statfs` would trigger it, so it is told
    /// by `/proc/self/mountinfo` alone, and only one test touches it.
    Autofs,
}

impl Mount {
    pub fn var(self) -> &'static str {
        match self {
            Mount::Btrfs => "DDC_MOUNT_BTRFS",
            Mount::BtrfsSubvolume => "DDC_MOUNT_BTRFS_SUBVOLUME",
            Mount::BtrfsSnapshotRo => "DDC_MOUNT_BTRFS_SNAPSHOT_RO",
            Mount::BtrfsSnapshotRw => "DDC_MOUNT_BTRFS_SNAPSHOT_RW",
            Mount::BtrfsTree => "DDC_MOUNT_BTRFS_TREE",
            Mount::BtrfsRemount => "DDC_MOUNT_BTRFS_REMOUNT",
            Mount::Xfs => "DDC_MOUNT_XFS",
            Mount::Casefold => "DDC_MOUNT_CASEFOLD",
            Mount::Exfat => "DDC_MOUNT_EXFAT",
            Mount::Tmpfs => "DDC_MOUNT_TMPFS",
            Mount::TmpfsRootTop => "DDC_MOUNT_TMPFS_ROOT_TOP",
            Mount::Zfs => "DDC_MOUNT_ZFS",
            Mount::ZfsChild => "DDC_MOUNT_ZFS_CHILD",
            Mount::ZfsTree => "DDC_MOUNT_ZFS_TREE",
            Mount::ZfsDelete => "DDC_MOUNT_ZFS_DELETE",
            Mount::ZfsGuard => "DDC_MOUNT_ZFS_GUARD",
            Mount::Nfs => "DDC_MOUNT_NFS",
            Mount::NfsSquash => "DDC_MOUNT_NFS_SQUASH",
            Mount::Cifs => "DDC_MOUNT_CIFS",
            Mount::Bind => "DDC_MOUNT_BIND",
            Mount::Autofs => "DDC_MOUNT_AUTOFS",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Mount::Btrfs => "btrfs",
            Mount::BtrfsSubvolume => "btrfs subvolume",
            Mount::BtrfsSnapshotRo => "btrfs read-only snapshot",
            Mount::BtrfsSnapshotRw => "btrfs writable snapshot",
            Mount::BtrfsTree => "btrfs tree",
            Mount::BtrfsRemount => "remounted btrfs",
            Mount::Xfs => "xfs",
            Mount::Casefold => "casefold ext4",
            Mount::Exfat => "exfat",
            Mount::Tmpfs => "tmpfs",
            Mount::TmpfsRootTop => "root-topped tmpfs",
            Mount::Zfs => "zfs",
            Mount::ZfsChild => "zfs child dataset",
            Mount::ZfsTree => "zfs tree",
            Mount::ZfsDelete => "zfs delete home",
            Mount::ZfsGuard => "zfs guard home",
            Mount::Nfs => "nfs",
            Mount::NfsSquash => "squashed nfs",
            Mount::Cifs => "cifs",
            Mount::Bind => "bind",
            Mount::Autofs => "autofs",
        }
    }

    /// `statfs`'s `f_type` of the filesystem (`linux/magic.h`). The casefold
    /// folder and the bind mount are ext4: their tests prove the rest.
    pub fn magic(self) -> u32 {
        match self {
            Mount::Btrfs
            | Mount::BtrfsTree
            | Mount::BtrfsRemount
            | Mount::BtrfsSubvolume
            | Mount::BtrfsSnapshotRo
            | Mount::BtrfsSnapshotRw => 0x9123_683E,
            Mount::Xfs => 0x5846_5342,
            Mount::Casefold | Mount::Bind => 0xEF53,
            Mount::Exfat => 0x2011_BAB0,
            Mount::Tmpfs | Mount::TmpfsRootTop => 0x0102_1994,
            Mount::Zfs | Mount::ZfsChild | Mount::ZfsTree | Mount::ZfsDelete | Mount::ZfsGuard => 0x2FC1_2FC1,
            Mount::Nfs | Mount::NfsSquash => 0x6969,
            Mount::Cifs => 0xFE53_4D42,
            Mount::Autofs => 0x0187,
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

/// The type of every mount whose mount point is `path`, oldest first, from
/// `/proc/self/mountinfo`: read without touching `path`, so an automount
/// point stays untriggered.
pub fn mounted_at(path: &std::path::Path) -> Vec<String> {
    let text = std::fs::read_to_string("/proc/self/mountinfo").expect("mountinfo");
    text.lines()
        .filter_map(|line| {
            let (left, right) = line.split_once(" - ")?;
            let point = left.split(' ').nth(4)?.replace("\\040", " ");
            (std::path::Path::new(&point) == path).then(|| right.split(' ').next().unwrap_or("").to_owned())
        })
        .collect()
}

/// `file`, inside a btrfs subvolume or a ZFS dataset, has one identity for
/// the guard, the walk, `lstat` and the process table, whose device is the
/// filesystem's `f_fsid` folded, never the anonymous `st_dev`; and a
/// process that holds it open is found holding it, so the guard's liveness
/// check calls a session of that file live (safety ADR, amendment of
/// 2026-11-02, "btrfs and ZFS on Linux", "Stable identities").
pub fn held_open_is_in_use(test: &str, file: &std::path::Path) {
    use std::os::unix::fs::MetadataExt as _;
    use std::time::{Duration, Instant, SystemTime};

    use ddc_core::agents::liveness::{self, FileFact, LiveBy, SessionFacts, SessionLiveness};
    use ddc_core::platform::{bulk, fsops::DirFd, meta, procs};
    use ddc_core::scan::Times;

    let (dir, name) = (file.parent().unwrap(), file.file_name().unwrap());
    let key = DirFd::open(dir).unwrap().stat_child(name).unwrap();
    let id = (key.dev, key.ino);
    let folded = rustix::fs::statvfs(file).unwrap().f_fsid;
    assert_eq!(id.0, folded, "{test}: the device is f_fsid folded");
    assert_ne!(id.0, std::fs::symlink_metadata(file).unwrap().dev(), "{test}: never the anonymous st_dev");
    assert_eq!(meta::lstat(file).unwrap().id(), id, "{test}: lstat");
    let listing = bulk::list(dir, Some(meta::lstat(dir).unwrap().id()), false, Times::None).unwrap();
    let entry = listing.entries.iter().find(|e| e.name == name).unwrap();
    assert_eq!((entry.dev, entry.ino), id, "{test}: the walk");

    let mut holder =
        std::process::Command::new("/bin/sh").args(["-c", "exec 3<\"$0\"; exec sleep 30"]).arg(file).spawn().unwrap();
    let pid = i32::try_from(holder.id()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let found = loop {
        if procs::open_vnodes(pid).iter().any(|v| (v.dev, v.ino) == id) {
            break true;
        }
        if Instant::now() > deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let seen = procs::open_vnodes(pid);
    // The guard's verdict on a session whose one file is that file, old
    // enough not to count as recent: live, because it is open.
    let session = SessionFacts { session: "s".into(), files: vec![FileFact { id, modified: Some(0) }] };
    let verdict = liveness::claude(Ok(&[][..]), &[session], &[pid], procs::open_vnodes, SystemTime::now());
    let _ = holder.kill();
    let _ = holder.wait();
    assert!(found, "{test}: {id:?} not among {seen:?}");
    assert_eq!(verdict, [SessionLiveness::Live { by: LiveBy::OpenFile }], "{test}: the guard's verdict");
}

/// Where the job made `kind`, or `None` after printing why the test skips.
pub fn mount(test: &str, kind: Mount) -> Option<PathBuf> {
    if kind == Mount::Autofs {
        // Not even `symlink_metadata`: std's `statx` passes no
        // `AT_NO_AUTOMOUNT`, so it would mount the map.
        let dir = variable(test, kind.var(), &format!("no {} mount", kind.name()))?;
        assert!(mounted_at(&dir).iter().any(|t| t == "autofs"), "{test}: {dir:?} is no autofs mount point");
        return Some(dir);
    }
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
    /// A home with an idle Node project whose `node_modules` holds a
    /// character device of the user's (`node_modules/dev/null`).
    DeviceTree,
    /// A folder of the user's on the user's own tmpfs holding a character
    /// device of theirs (`cache/null`), for a move into that tmpfs's Trash.
    DeviceTrash,
    /// A home with five idle Node projects: `p1`'s `node_modules` holds a
    /// `chattr +i` file, `p2`'s a `chattr +a` file, `p3`'s a `chattr +i`
    /// folder, `p4`'s is `chattr +i` itself, and `p5`'s holds none.
    ImmutableTree,
}

impl Prepared {
    pub fn var(self) -> &'static str {
        match self {
            Prepared::Device => "DDC_FIXTURE_DEVICE",
            Prepared::Immutable => "DDC_FIXTURE_IMMUTABLE",
            Prepared::AppendOnly => "DDC_FIXTURE_APPEND_ONLY",
            Prepared::DeviceTree => "DDC_FIXTURE_DEVICE_TREE",
            Prepared::DeviceTrash => "DDC_FIXTURE_DEVICE_TRASH",
            Prepared::ImmutableTree => "DDC_FIXTURE_IMMUTABLE_TREE",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Prepared::Device => "device",
            Prepared::Immutable => "immutable",
            Prepared::AppendOnly => "append-only",
            Prepared::DeviceTree => "device tree",
            Prepared::DeviceTrash => "device trash",
            Prepared::ImmutableTree => "immutable tree",
        }
    }
}

/// A file only root can make, which the job prepared, each in a folder of
/// its own; `None` after printing why the test skips.
pub fn prepared(test: &str, what: Prepared) -> Option<PathBuf> {
    given(test, what.var(), &format!("no {} fixture", what.name()))
}

fn given(test: &str, var: &str, missing: &str) -> Option<PathBuf> {
    let path = variable(test, var, missing)?;
    assert!(std::fs::symlink_metadata(&path).is_ok(), "{test}: {var} = {path:?} does not exist");
    Some(path)
}

#[allow(clippy::disallowed_methods)] // a switch of the test harness, not of the scanner
fn variable(test: &str, var: &str, missing: &str) -> Option<PathBuf> {
    match std::env::var_os(var).filter(|v| !v.is_empty()) {
        None => {
            eprintln!("skipped {test}: {missing} ({var} unset)");
            None
        }
        Some(v) => Some(PathBuf::from(v)),
    }
}
