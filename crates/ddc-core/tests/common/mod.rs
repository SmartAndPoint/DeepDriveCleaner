//! Temporary directory trees for scanner tests. Trees live under the system
//! temp dir and never inside the real `$HOME`.
#![allow(dead_code)]

pub mod fakes;
pub mod fixtures;

use std::cell::RefCell;
use std::fs::{self, File};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use ddc_core::platform::meta;

/// 2024-01-01, Unix seconds: the time `ddc-loadgen` gives its trees, and
/// [`TreeBuilder::age`]'s usual "long ago".
pub const OLD: i64 = 1_704_067_200;

pub struct TreeBuilder {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    locked: RefCell<Vec<PathBuf>>,
}

impl TreeBuilder {
    #[allow(clippy::disallowed_methods)] // guard against writing into the real HOME
    pub fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path().canonicalize().expect("canonical tempdir");
        if let Some(home) = std::env::var_os("HOME").and_then(|h| PathBuf::from(h).canonicalize().ok()) {
            assert!(!root.starts_with(&home), "test tree {root:?} must not live inside the real HOME");
        }
        Self { _tmp: tmp, root, locked: RefCell::new(Vec::new()) }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    pub fn dir(&self, rel: &str) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(&p).expect("create dir");
        p
    }

    /// A file of `len` non-zero bytes (fully allocated).
    pub fn file(&self, rel: &str, len: usize) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let mut f = File::create(&p).expect("create file");
        let chunk: Vec<u8> = (0..len.min(1 << 16)).map(|i| (i % 251) as u8 + 1).collect();
        let mut left = len;
        while left > 0 {
            let n = left.min(chunk.len());
            f.write_all(&chunk[..n]).unwrap();
            left -= n;
        }
        f.sync_all().unwrap();
        p
    }

    /// A file with the given text content.
    pub fn write(&self, rel: &str, content: &str) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, content).expect("write file");
        p
    }

    /// A sparse file: logical size `len`, nothing allocated.
    pub fn sparse(&self, rel: &str, len: u64) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        File::create(&p).unwrap().set_len(len).unwrap();
        p
    }

    pub fn symlink(&self, target: impl AsRef<Path>, rel: &str) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        symlink(target, &p);
        p
    }

    pub fn hardlink(&self, existing: impl AsRef<Path>, rel: &str) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::hard_link(existing, &p).expect("hard link");
        p
    }

    /// APFS clone (`cp -c`): shares all blocks with `src`.
    pub fn clone_file(&self, src: impl AsRef<Path>, rel: &str) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let ok = Command::new("/bin/cp").arg("-c").arg(src.as_ref()).arg(&p).status().expect("cp -c").success();
        assert!(ok, "cp -c failed");
        p
    }

    /// Sets the modification time (and the date added) of `rel` alone to
    /// `secs`, Unix seconds, without following a symlink.
    pub fn touch(&self, rel: &str, secs: i64) {
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64);
        ddc_core::platform::fsops::set_times(&self.path(rel), at, at).expect("set times");
    }

    /// [`Self::touch`] on `rel` and on everything under it.
    pub fn age(&self, rel: &str, secs: i64) {
        let mut stack = vec![self.path(rel)];
        while let Some(p) = stack.pop() {
            if fs::symlink_metadata(&p).unwrap().is_dir() {
                stack.extend(fs::read_dir(&p).unwrap().map(|e| e.unwrap().path()));
            }
            self.touch(p.strip_prefix(&self.root).unwrap().to_str().unwrap(), secs);
        }
    }

    /// Removes all permissions from a directory; restored on drop.
    pub fn lock(&self, rel: &str) -> PathBuf {
        let p = self.path(rel);
        set_mode(&p, 0o000);
        self.locked.borrow_mut().push(p.clone());
        p
    }
}

impl Drop for TreeBuilder {
    fn drop(&mut self) {
        for p in self.locked.borrow().iter() {
            let _ = try_set_mode(p, 0o755);
        }
    }
}

/// True, after printing why, when the build's platform is not supported
/// (ADR "Platform seam: one platform module per OS with declared
/// capabilities", §9): a test of a gated entry point that will run on every
/// platform once its switch flips starts with
/// `if common::unsupported("name") { return; }`. On macOS it is never true:
/// `tests/platform.rs` asserts `SUPPORTED`.
pub fn unsupported(test: &str) -> bool {
    match ddc_core::platform::require_supported() {
        Ok(()) => false,
        Err(why) => {
            eprintln!("skipped {test}: {why}");
            true
        }
    }
}

/// Allocated bytes of one path, without following symlinks.
pub fn alloc(path: &Path) -> u64 {
    meta::alloc_bytes(&fs::symlink_metadata(path).unwrap())
}

/// `(dev, ino)` of one path, without following symlinks.
pub fn id(path: &Path) -> (u64, u64) {
    meta::file_id(&fs::symlink_metadata(path).unwrap()).unwrap()
}

/// Sets the Unix permission bits of `path`. The harness's Unix-only helpers
/// (this one and [`symlink`]) keep one signature on every OS and panic
/// elsewhere: only a gated test reaches them, after it has skipped there.
pub fn set_mode(path: &Path, mode: u32) {
    try_set_mode(path, mode).unwrap_or_else(|e| panic!("chmod {mode:o} {}: {e}", path.display()));
}

#[cfg(unix)]
fn try_set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn try_set_mode(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "Unix permission bits"))
}

/// A symlink at `link` to `target`; Unix only, as [`set_mode`].
#[cfg(unix)]
pub fn symlink(target: impl AsRef<Path>, link: impl AsRef<Path>) {
    let link = link.as_ref();
    std::os::unix::fs::symlink(target, link).unwrap_or_else(|e| panic!("symlink {}: {e}", link.display()));
}

#[cfg(not(unix))]
pub fn symlink(_target: impl AsRef<Path>, link: impl AsRef<Path>) {
    panic!("symlink {}: Unix only", link.as_ref().display());
}

/// Reference size: sum of `blocks * 512` of every non-directory under `root`,
/// hard links counted once, symlinks not followed.
pub fn reference_alloc(root: &Path) -> u64 {
    let mut seen = std::collections::HashSet::new();
    let mut total = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).unwrap() {
            let e = e.unwrap();
            let md = e.metadata().unwrap();
            if md.is_dir() {
                stack.push(e.path());
            } else if meta::link_count(&md) <= 1 || seen.insert(meta::file_id(&md).unwrap()) {
                total += meta::alloc_bytes(&md);
            }
        }
    }
    total
}
