//! Temporary directory trees for scanner tests. Trees live under the system
//! temp dir and never inside the real `$HOME`.
#![allow(dead_code)]

pub mod fakes;
pub mod fixtures;

use std::cell::RefCell;
use std::fs::{self, File};
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::Command;

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
        std::os::unix::fs::symlink(target, &p).expect("symlink");
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

    /// Removes all permissions from a directory; restored on drop.
    pub fn lock(&self, rel: &str) -> PathBuf {
        let p = self.path(rel);
        fs::set_permissions(&p, fs::Permissions::from_mode(0o000)).unwrap();
        self.locked.borrow_mut().push(p.clone());
        p
    }
}

impl Drop for TreeBuilder {
    fn drop(&mut self) {
        for p in self.locked.borrow().iter() {
            let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o755));
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
    fs::symlink_metadata(path).unwrap().blocks() * 512
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
            } else if md.nlink() <= 1 || seen.insert((md.dev(), md.ino())) {
                total += md.blocks() * 512;
            }
        }
    }
    total
}
