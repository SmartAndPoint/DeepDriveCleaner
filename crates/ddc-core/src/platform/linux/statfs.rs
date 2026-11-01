//! Volume statistics on Linux: the sizes from `statvfs(2)`, and the file
//! system's type, mount point and source from `/proc/self/mountinfo`,
//! which `statvfs` does not give.

use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

pub use crate::platform::types::VolumeStats;

/// One line of `mountinfo`: what `statvfs` leaves out.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Mount {
    mount_point: PathBuf,
    fs_type: String,
    source: String,
}

pub fn statfs(path: &Path) -> io::Result<VolumeStats> {
    let st = rustix::fs::statvfs(path)?;
    let mounts = parse(&read_mountinfo()?);
    // The mount a path lies on is the one with the longest mount point above
    // it; the canonical path, so a symlinked folder counts where it lands.
    let real = super::paths::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let m = mount_of(&mounts, &real).ok_or_else(|| io::Error::other("no mount above the path in mountinfo"))?;
    Ok(stats(&st, m))
}

/// Every mounted file system, as `mountinfo` lists them; one `statvfs`
/// refuses (a mount point the user cannot reach) is left out.
pub fn mounts() -> io::Result<Vec<VolumeStats>> {
    let mounts = parse(&read_mountinfo()?);
    Ok(mounts.iter().filter_map(|m| rustix::fs::statvfs(&m.mount_point).ok().map(|st| stats(&st, m))).collect())
}

fn read_mountinfo() -> io::Result<Vec<u8>> {
    std::fs::read("/proc/self/mountinfo")
}

fn stats(st: &rustix::fs::StatVfs, m: &Mount) -> VolumeStats {
    // Block counts are in fragment units (`f_frsize`), not `f_bsize`.
    let block = if st.f_frsize == 0 { st.f_bsize } else { st.f_frsize };
    VolumeStats {
        fs_type: m.fs_type.clone(),
        mount_point: m.mount_point.clone(),
        available_bytes: st.f_bavail.saturating_mul(block),
        total_bytes: st.f_blocks.saturating_mul(block),
        free_bytes: st.f_bfree.saturating_mul(block),
        mounted_from: m.source.clone(),
    }
}

/// The mount whose mount point is the longest one above `path`, by whole
/// components; of equal ones the last listed, which is mounted over the rest.
fn mount_of<'a>(mounts: &'a [Mount], path: &Path) -> Option<&'a Mount> {
    let mut best: Option<&Mount> = None;
    for m in mounts.iter().filter(|m| path.starts_with(&m.mount_point)) {
        if best.is_none_or(|b| m.mount_point.components().count() >= b.mount_point.components().count()) {
            best = Some(m);
        }
    }
    best
}

/// Parses `mountinfo` (proc(5)): `id parent major:minor root mount-point
/// options [optional fields…] - fs-type source super-options`. A line it
/// cannot read is skipped.
fn parse(text: &[u8]) -> Vec<Mount> {
    text.split(|&b| b == b'\n')
        .filter_map(|line| {
            let fields: Vec<&[u8]> = line.split(|&b| b == b' ').collect();
            let dash = fields.iter().position(|f| *f == b"-")?;
            let mount_point = fields.get(4).filter(|_| dash > 5)?;
            let fs_type = fields.get(dash + 1)?;
            let source = fields.get(dash + 2)?;
            Some(Mount {
                mount_point: PathBuf::from(OsStr::from_bytes(&unescape(mount_point))),
                fs_type: String::from_utf8_lossy(&unescape(fs_type)).into_owned(),
                source: String::from_utf8_lossy(&unescape(source)).into_owned(),
            })
        })
        .collect()
}

/// The kernel writes a space, tab, newline and backslash in a field as
/// `\040`, `\011`, `\012` and `\134`.
fn unescape(field: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(field.len());
    let mut i = 0;
    while i < field.len() {
        let octal =
            field.get(i + 1..i + 4).filter(|d| field[i] == b'\\' && d.iter().all(|c| (b'0'..=b'7').contains(c)));
        match octal {
            Some(d) => {
                out.push((d[0] - b'0') * 64 + (d[1] - b'0') * 8 + (d[2] - b'0'));
                i += 4;
            }
            None => {
                out.push(field[i]);
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const MOUNTINFO: &[u8] = b"\
22 1 259:2 / / rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw
23 22 0:21 / /proc rw,nosuid,nodev,noexec,relatime shared:12 - proc proc rw
40 22 259:3 / /home rw,relatime shared:30 master:2 - btrfs /dev/nvme0n1p3 rw,subvol=/home
41 40 259:3 /data /home/u/My\\040Disk rw,relatime shared:30 - btrfs /dev/nvme0n1p3 rw,subvol=/data
52 22 0:45 / /var/lib/docker/overlay2/x/merged rw,relatime - overlay overlay rw,lowerdir=/a:/b
60 22 259:2 /srv /mnt/bind rw,relatime shared:1 - ext4 /dev/nvme0n1p2 rw
61 22 0:50 / /home rw - tmpfs tmpfs rw
garbage line
";

    #[test]
    fn mountinfo_is_read_with_its_optional_fields_and_escapes() {
        let m = parse(MOUNTINFO);
        assert_eq!(m.len(), 7, "the garbage line is skipped");
        assert_eq!(m[0], Mount { mount_point: "/".into(), fs_type: "ext4".into(), source: "/dev/nvme0n1p2".into() });
        assert_eq!(
            (m[2].mount_point.as_path(), m[2].fs_type.as_str()),
            (Path::new("/home"), "btrfs"),
            "two optional fields"
        );
        assert_eq!(m[3].mount_point, Path::new("/home/u/My Disk"), "\\040 is a space");
        assert_eq!((m[4].fs_type.as_str(), m[4].source.as_str()), ("overlay", "overlay"));
        assert_eq!(unescape(b"a\\011b\\134c\\9"), b"a\tb\\c\\9");
    }

    #[test]
    fn a_path_lies_on_the_longest_mount_above_it_and_the_last_one_mounted() {
        let m = parse(MOUNTINFO);
        let on = |p: &str| {
            mount_of(&m, Path::new(p)).map(|m| (m.mount_point.to_str().unwrap().to_owned(), m.fs_type.clone()))
        };
        assert_eq!(on("/etc/hosts"), Some(("/".into(), "ext4".into())));
        assert_eq!(on("/home/u/My Disk/x"), Some(("/home/u/My Disk".into(), "btrfs".into())));
        assert_eq!(
            on("/home/u/My Diskette"),
            Some(("/home".into(), "tmpfs".into())),
            "whole components; the later /home wins"
        );
        assert_eq!(on("/mnt/bind/f"), Some(("/mnt/bind".into(), "ext4".into())));
    }

    #[test]
    fn the_root_and_a_temp_folder_have_a_mount_with_sizes() {
        let root = statfs(Path::new("/")).unwrap();
        assert_eq!(root.mount_point, Path::new("/"));
        assert!(root.total_bytes > 0 && root.available_bytes <= root.total_bytes && !root.fs_type.is_empty());
        let d = tempfile::tempdir().unwrap();
        let tmp = statfs(d.path()).unwrap();
        assert!(super::super::paths::canonicalize(d.path()).unwrap().starts_with(&tmp.mount_point));
        assert!(mounts().unwrap().iter().any(|m| m.mount_point == Path::new("/")));
    }
}
