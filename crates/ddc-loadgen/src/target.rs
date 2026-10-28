//! Where a tree may be built, decided by file system identity, never by a
//! path prefix. Two kinds of target are allowed: a fresh folder under the
//! system temporary folder holding the generator's marker, and a disk image
//! the generator attached itself. Anything on the file system of the real
//! home or of `/` that is not the marked folder is refused.

use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use ddc_core::sys::{procs, statfs};

/// Length of the tree's absolute path, after `canonicalize()`: path lengths,
/// the `PATH_MAX` boundary and the order of findings are then the same on
/// every machine.
pub const ROOT_LEN: usize = 160;
/// Prefix of a marked temporary folder's name.
pub const PREFIX: &str = "ddc-load-";
/// The marker file; it holds the folder name's random suffix.
pub const MARKER: &str = ".ddc-loadgen";

/// Facts about a target, read from the system by [`facts`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub target_dev: u64,
    /// Device of the real home (password database).
    pub home_dev: u64,
    pub root_dev: u64,
    /// A direct child of the temporary folder named `ddc-load-<nonce>`,
    /// owned by this user, with a marker that holds the nonce.
    pub marked_temp: bool,
    /// For an image target: what `statfs` says is mounted there, and the
    /// device `hdiutil attach` returned.
    pub image: Option<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// An image target whose mount is not the attached device.
    WrongDevice { mounted: String, expected: String },
    /// An image target on the home's or the boot volume's file system.
    ImageOnSystemVolume,
    /// Neither a marked temporary folder nor an image.
    Unmarked,
}

impl Refusal {
    pub fn message(&self) -> String {
        match self {
            Self::WrongDevice { mounted, expected } => {
                format!("the target is mounted from {mounted}, not from the attached image {expected}")
            }
            Self::ImageOnSystemVolume => {
                "the image target is on the home's or the boot volume's file system".to_owned()
            }
            Self::Unmarked => format!(
                "the target is neither a marked temporary folder ({PREFIX}* with {MARKER}, from `ddc-loadgen mktemp`) \
                 nor an attached image (--device)"
            ),
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for Refusal {}

/// The guard's decision, a pure function of the facts.
pub fn decide(f: &Facts) -> Result<(), Refusal> {
    let on_system = f.target_dev == f.root_dev || f.target_dev == f.home_dev;
    match &f.image {
        Some((mounted, expected)) if mounted != expected => {
            Err(Refusal::WrongDevice { mounted: mounted.clone(), expected: expected.clone() })
        }
        Some(_) if on_system => Err(Refusal::ImageOnSystemVolume),
        Some(_) => Ok(()),
        None if f.marked_temp => Ok(()),
        None => Err(Refusal::Unmarked),
    }
}

/// Reads the facts for `target`; `device` is the image's device for an image target.
/// Without a real home to compare with, it fails rather than guess.
pub fn facts(target: &Path, device: Option<&str>) -> io::Result<Facts> {
    let target = target.canonicalize()?;
    let home = procs::real_home().ok_or_else(|| io::Error::other("the password database names no home folder"))?;
    let dev = |p: &Path| fs::metadata(p).map(|m| m.dev());
    let image = match device {
        Some(d) => Some((statfs::statfs(&target)?.mounted_from, d.to_owned())),
        None => None,
    };
    Ok(Facts {
        target_dev: dev(&target)?,
        home_dev: dev(&home)?,
        root_dev: dev(Path::new("/"))?,
        marked_temp: marked_temp(&target),
        image,
    })
}

fn marked_temp(target: &Path) -> bool {
    // `$TMPDIR`, read outside `Env`: it only narrows where a tree may go.
    std::env::temp_dir().canonicalize().is_ok_and(|tmp| marked_in(target, &tmp))
}

/// Whether `target` is a marked folder directly in `tmp` (canonical).
fn marked_in(target: &Path, tmp: &Path) -> bool {
    let nonce = target.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix(PREFIX));
    let marker = target.join(MARKER);
    target.parent() == Some(tmp)
        && nonce.is_some_and(|n| !n.is_empty())
        && fs::symlink_metadata(target).is_ok_and(|m| m.is_dir() && m.uid() == procs::uid())
        && fs::symlink_metadata(&marker).is_ok_and(|m| m.is_file() && m.uid() == procs::uid())
        && fs::read_to_string(&marker).ok().as_deref() == nonce
}

/// Creates a marked temporary folder that stays after the run; returns its canonical path.
pub fn mktemp() -> io::Result<PathBuf> {
    Ok(marked()?.keep())
}

/// A marked temporary folder, removed with everything in it when dropped (tests).
pub fn marked() -> io::Result<tempfile::TempDir> {
    let dir = tempfile::Builder::new().prefix(PREFIX).tempdir_in(std::env::temp_dir().canonicalize()?)?;
    let nonce =
        dir.path().file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix(PREFIX)).unwrap_or_default();
    fs::write(dir.path().join(MARKER), nonce)?;
    Ok(dir)
}

/// A [`marked`] folder that gives back the permissions a profile locked
/// before it is removed (tests): a failing test still leaves nothing behind.
pub struct Scratch(tempfile::TempDir);

impl Scratch {
    pub fn new() -> io::Result<Self> {
        marked().map(Self)
    }

    pub fn path(&self) -> &Path {
        self.0.path()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        unlock(self.0.path());
    }
}

/// Removes every marked temporary folder of this user (`ddc-load-*` with
/// its marker, by the same rule as the guard); returns what it removed.
/// It cannot tell an abandoned tree from one a run in another worktree is
/// still using: run it when no load test or build is running.
pub fn clean() -> io::Result<Vec<PathBuf>> {
    clean_in(&std::env::temp_dir().canonicalize()?)
}

/// [`clean`] in the canonical folder `tmp`. A folder that cannot be removed
/// does not stop the others; the first failure is reported at the end.
#[allow(clippy::disallowed_methods)] // only folders the guard accepts as the generator's own
pub fn clean_in(tmp: &Path) -> io::Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    let mut failed: Vec<(PathBuf, io::Error)> = Vec::new();
    for entry in fs::read_dir(tmp)? {
        let path = entry?.path();
        let ours = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(PREFIX));
        if ours && marked_in(&path, tmp) {
            unlock(&path);
            match fs::remove_dir_all(&path) {
                Ok(()) => removed.push(path),
                Err(e) => failed.push((path, e)),
            }
        }
    }
    match failed.first() {
        None => Ok(removed),
        Some((path, e)) => Err(io::Error::other(format!(
            "removed {}, could not remove {} folder(s); first {}: {e}",
            removed.len(),
            failed.len(),
            path.display()
        ))),
    }
}

/// Gives back the permissions of folders a profile locked (`hostile`), so
/// the tree can be removed. Paths past `PATH_MAX` are not followed; nothing
/// is locked there.
pub fn unlock(dir: &Path) {
    let Ok(md) = fs::symlink_metadata(dir) else { return };
    if !md.is_dir() || md.uid() != procs::uid() {
        return;
    }
    if md.mode() & 0o700 != 0o700 {
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o755));
    }
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            unlock(&entry.path());
        }
    }
}

/// The folders of one run inside a checked target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The scan root, [`ROOT_LEN`] bytes long.
    pub tree: PathBuf,
    /// An empty home beside the tree, for profiles without their own.
    pub home: PathBuf,
    /// `--data-dir` of every run.
    pub data: PathBuf,
}

/// Checks `target` with the guard, then lays out a run in it: a padding
/// folder that brings `<target>/<pad>/tree` to [`ROOT_LEN`] bytes, and the
/// tree, home and data folders in it. The tree must not exist yet.
pub fn prepare(target: &Path, device: Option<&str>) -> anyhow::Result<Layout> {
    let target = target.canonicalize()?;
    decide(&facts(&target, device)?)?;
    let room = ROOT_LEN.checked_sub(target.as_os_str().len() + "/".len() + "/tree".len()).filter(|&n| n > 0);
    let room = room.ok_or_else(|| {
        anyhow::anyhow!("the target path {} is too long for a {ROOT_LEN}-byte tree root", target.display())
    })?;
    let base = target.join(pad(room));
    let layout = Layout { tree: base.join("tree"), home: base.join("home"), data: base.join("data") };
    fs::create_dir_all(&base)?;
    fs::create_dir(&layout.tree)?;
    fs::create_dir_all(&layout.home)?;
    fs::create_dir_all(&layout.data)?;
    debug_assert_eq!(layout.tree.as_os_str().len(), ROOT_LEN);
    Ok(layout)
}

/// A relative path of exactly `len` bytes: `p` names of at most 200 bytes.
fn pad(len: usize) -> PathBuf {
    let mut s = String::new();
    let mut left = len;
    while left > 0 {
        if !s.is_empty() {
            s.push('/');
            left -= 1;
        }
        let mut take = left.min(200);
        if left - take == 1 {
            // A lone byte left over would need a separator and an empty name.
            take -= 1;
        }
        s.extend(std::iter::repeat_n('p', take));
        left -= take;
    }
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_has_the_exact_length() {
        for len in [1, 2, 199, 200, 201, 202, 401, 402, 1000] {
            let p = pad(len);
            assert_eq!(p.as_os_str().len(), len, "{len}");
            assert!(p.components().all(|c| !c.as_os_str().is_empty() && c.as_os_str().len() <= 200), "{len}");
        }
    }
}
