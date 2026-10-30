//! The load generator on macOS and Linux. The builder makes every call
//! relative to an open parent directory (`mkdirat`, `openat`, `linkat`,
//! `symlinkat`, `utimensat`), so a path longer than `PATH_MAX` can be built.
//! Modification times are set in a second pass, after every child exists.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, Metadata};
use std::io::{self, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::process::ExitStatusExt as _;
use std::path::Path;
use std::process::ExitStatus;

use ddc_core::platform::procs;
use rustix::fs::{
    AtFlags, Mode, OFlags, Timespec, Timestamps, UTIME_OMIT, linkat, mkdirat, openat, symlinkat, utimensat,
};

use crate::build::{content, padded};
use crate::desc::{Age, Desc, Kind, Node, OLD_SECS, name as leaf, parent};

/// The largest tree a marked temporary folder takes (the S tier): millions
/// of entries never land on the user's volume. Linux has no image
/// (DDC-007), so there the folder takes any size.
pub const TEMP_MAX: Option<u64> = if cfg!(target_os = "macos") { Some(10_000) } else { None };

/// Directories are opened with this; nothing is followed.
const DIR: OFlags = OFlags::RDONLY.union(OFlags::DIRECTORY).union(OFlags::NOFOLLOW).union(OFlags::CLOEXEC);

/// This platform has a builder.
pub fn builds() -> io::Result<()> {
    Ok(())
}

/// Builds `desc` inside the empty directory `tree`.
pub fn build(desc: &Desc, tree: &Path) -> io::Result<()> {
    // One handle per open ancestor: a chain to `PATH_MAX` is about 430 deep,
    // past the default soft limit of 256.
    ddc_core::platform::rlimit::raise_nofile_limit()?;
    let root = openat(rustix::fs::CWD, tree, DIR, Mode::empty())?;
    let block = content();
    walk(&root, &desc.nodes, |dir, node| create(dir, node, tree, &block))?;
    // Hard links last: a link may sort before the file it names.
    walk(&root, &desc.nodes, |dir, node| match &node.kind {
        Kind::HardLink(target) => {
            linkat(&root, target.as_slice(), dir, leaf(&node.path), AtFlags::empty()).map_err(io::Error::from)
        }
        _ => Ok(()),
    })?;
    walk(&root, &desc.nodes, touch)?;
    // Last, so their contents and times are already in place.
    for path in &desc.locked {
        fs::set_permissions(tree.join(OsStr::from_bytes(path)), fs::Permissions::from_mode(0o000))?;
    }
    Ok(())
}

/// Visits the nodes in order with their parent directory open. The nodes
/// are sorted depth-first (`Desc::finish`), so the parent is always on
/// the stack of open directories.
fn walk(root: &OwnedFd, nodes: &[Node], mut visit: impl FnMut(&OwnedFd, &Node) -> io::Result<()>) -> io::Result<()> {
    let mut open: Vec<(&[u8], OwnedFd)> = Vec::new();
    for node in nodes {
        let up = parent(&node.path).unwrap_or_default();
        while open.last().is_some_and(|(p, _)| *p != up) {
            open.pop();
        }
        let dir = match open.last() {
            Some((_, fd)) => fd,
            None if up.is_empty() => root,
            None => return Err(io::Error::other(format!("{} has no parent", String::from_utf8_lossy(&node.path)))),
        };
        visit(dir, node)?;
        if node.kind == Kind::Dir {
            let fd = openat(dir, leaf(&node.path), DIR, Mode::empty())?;
            open.push((&node.path, fd));
        }
    }
    Ok(())
}

fn create(dir: &OwnedFd, node: &Node, tree: &Path, block: &[u8]) -> io::Result<()> {
    let at = leaf(&node.path);
    let new_file = || {
        let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        openat(dir, at, flags, Mode::from_raw_mode(0o644)).map(File::from)
    };
    match &node.kind {
        Kind::Dir => mkdirat(dir, at, Mode::from_raw_mode(0o755))?,
        Kind::File { blocks } => {
            let mut f = new_file()?;
            for _ in 0..*blocks {
                f.write_all(block)?;
            }
        }
        Kind::Text(text) => new_file()?.write_all(&padded(text, tree)?)?,
        Kind::Symlink(target) => symlinkat(target.as_slice(), dir, at)?,
        Kind::HardLink(_) => {}
    }
    Ok(())
}

fn touch(dir: &OwnedFd, node: &Node) -> io::Result<()> {
    if node.age == Age::Fresh || matches!(node.kind, Kind::HardLink(_)) {
        // Fresh entries keep the build time; a hard link shares its file's times.
        return Ok(());
    }
    let old = Timespec { tv_sec: OLD_SECS, tv_nsec: 0 };
    let times = Timestamps { last_access: Timespec { tv_sec: 0, tv_nsec: UTIME_OMIT }, last_modification: old };
    utimensat(dir, leaf(&node.path), &times, AtFlags::SYMLINK_NOFOLLOW).map_err(io::Error::from)
}

/// `bytes` as a name: any bytes but NUL and `/` are one here.
pub fn name(bytes: &[u8]) -> Option<OsString> {
    Some(OsString::from_vec(bytes.to_vec()))
}

/// The signal that ended the process, if one did.
pub fn exit_signal(status: &ExitStatus) -> Option<i32> {
    status.signal()
}

/// Owned by the user running the generator.
pub fn is_mine(md: &Metadata) -> bool {
    md.uid() == procs::uid()
}

/// The owner may read, write and enter it.
pub fn owner_has_rwx(md: &Metadata) -> bool {
    md.mode() & 0o700 == 0o700
}

/// Gives a folder a profile locked its permissions back (`0755`).
pub fn unlock_dir(dir: &Path) -> io::Result<()> {
    fs::set_permissions(dir, fs::Permissions::from_mode(0o755))
}
