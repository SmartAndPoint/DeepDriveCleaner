//! What a btrfs subvolume is, through `BTRFS_IOC_GET_SUBVOL_INFO` (kernel
//! 4.18; no capability needed): a plain subvolume, which the walk enters,
//! or a snapshot, which it never does (safety ADR, amendment of 2026-11-02,
//! "btrfs and ZFS on Linux").

use std::os::fd::{AsRawFd as _, OwnedFd};

/// `struct btrfs_ioctl_get_subvol_info_args` of `linux/btrfs.h`, read only
/// for its three UUIDs. The request number encodes the size, so a layout the
/// kernel does not know is refused (`ENOTTY`), never misread.
#[repr(C)]
#[allow(dead_code)] // the kernel fills every field; only the UUIDs are read
struct SubvolInfo {
    treeid: u64,
    name: [u8; 256],
    parent_id: u64,
    dirid: u64,
    generation: u64,
    flags: u64,
    uuid: [u8; 16],
    parent_uuid: [u8; 16],
    received_uuid: [u8; 16],
    ctransid: u64,
    otransid: u64,
    stransid: u64,
    rtransid: u64,
    /// `ctime`, `otime`, `stime`, `rtime`: `{ __u64 sec; __u32 nsec; }`
    /// each, 16 bytes with its padding.
    times: [[u64; 2]; 4],
    reserved: [u64; 8],
}

const _: () = assert!(std::mem::size_of::<SubvolInfo>() == 504);
const _: () = assert!(std::mem::offset_of!(SubvolInfo, parent_uuid) == 312);
const _: () = assert!(std::mem::offset_of!(SubvolInfo, received_uuid) == 328);

/// `_IOR(BTRFS_IOCTL_MAGIC, 60, struct btrfs_ioctl_get_subvol_info_args)`.
const GET_SUBVOL_INFO: libc::c_ulong = 0x81f8_943c;

/// What a subvolume root is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Subvolume {
    /// Made by `btrfs subvolume create`: entered.
    Plain,
    /// A snapshot (`parent_uuid` set, read-only or writable) or a received
    /// copy (`received_uuid` set): never entered.
    Snapshot,
}

/// `dir`, a folder opened on a btrfs subvolume's root, as the kernel says.
/// Any error (`ENOTTY` below 4.18 or for an unknown layout, `EPERM`, not
/// btrfs) is returned, and the caller treats it as another filesystem.
pub(super) fn subvolume(dir: &OwnedFd) -> std::io::Result<Subvolume> {
    // SAFETY: an all-zero bit pattern is a valid value of this plain-data
    // struct of integers and byte arrays.
    let mut info: SubvolInfo = unsafe { std::mem::zeroed() };
    // SAFETY: `dir` is an open descriptor; `info` is a writable struct of
    // exactly the size the request number encodes, as the kernel expects.
    let done = unsafe { libc::ioctl(dir.as_raw_fd(), GET_SUBVOL_INFO, &raw mut info) };
    // Only a negative return is an error: the kernel can hand back a
    // positive leftover of its tree search with a filled answer, as
    // btrfs-progs, which tests `< 0`, expects.
    if done < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(kind(&info.parent_uuid, &info.received_uuid))
}

fn kind(parent_uuid: &[u8; 16], received_uuid: &[u8; 16]) -> Subvolume {
    if parent_uuid.iter().chain(received_uuid).any(|b| *b != 0) { Subvolume::Snapshot } else { Subvolume::Plain }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A snapshot keeps its source's UUID for life; a received copy carries
    /// the sender's. Either makes it a snapshot.
    #[test]
    fn a_parent_or_received_uuid_makes_a_snapshot() {
        let zero = [0u8; 16];
        let mut set = [0u8; 16];
        set[15] = 1;
        assert_eq!(kind(&zero, &zero), Subvolume::Plain);
        assert_eq!(kind(&set, &zero), Subvolume::Snapshot);
        assert_eq!(kind(&zero, &set), Subvolume::Snapshot);
    }

    /// Off btrfs the ioctl is refused, never read as a plain subvolume.
    #[test]
    fn off_btrfs_the_ioctl_is_an_error() {
        let d = tempfile::tempdir().unwrap();
        let fd = rustix::fs::open(
            d.path(),
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::DIRECTORY | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .unwrap();
        let on_btrfs = super::super::statfs::statfs(d.path()).is_ok_and(|v| v.fs_type == "btrfs");
        if !on_btrfs {
            assert!(subvolume(&fd).is_err());
        }
    }
}
