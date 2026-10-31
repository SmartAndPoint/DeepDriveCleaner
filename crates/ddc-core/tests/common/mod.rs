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

    /// [`Self::symlink`], or `Unsupported` where the harness makes none
    /// ([`try_symlink`]): for a test that does not skip otherwise.
    pub fn try_symlink(&self, target: impl AsRef<Path>, rel: &str) -> std::io::Result<PathBuf> {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap())?;
        try_symlink(target, &p)?;
        Ok(p)
    }

    pub fn hardlink(&self, existing: impl AsRef<Path>, rel: &str) -> PathBuf {
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::hard_link(existing, &p).expect("hard link");
        p
    }

    /// APFS clone (`cp -c`): shares all blocks with `src`.
    pub fn clone_file(&self, src: impl AsRef<Path>, rel: &str) -> PathBuf {
        self.try_clone_file(src, rel).expect("cp -c")
    }

    /// [`Self::clone_file`], or `Unsupported` where the platform makes no
    /// clones with `cp -c` (the test then skips through [`skipped_without`]).
    pub fn try_clone_file(&self, src: impl AsRef<Path>, rel: &str) -> std::io::Result<PathBuf> {
        use ddc_core::platform::{PLATFORM, Platform};
        if PLATFORM != Platform::Macos {
            return Err(std::io::Error::new(std::io::ErrorKind::Unsupported, format!("{PLATFORM} has no cp -c")));
        }
        let p = self.path(rel);
        fs::create_dir_all(p.parent().unwrap())?;
        let ok = Command::new("/bin/cp").arg("-c").arg(src.as_ref()).arg(&p).status()?.success();
        if ok { Ok(p) } else { Err(std::io::Error::other("cp -c failed")) }
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

/// True, after printing why, when the build's platform does not declare
/// `capability` (ADR "Platform seam: one platform module per OS with
/// declared capabilities", §9, amended): a test of what the capability
/// covers (Homebrew, the whole disk, the coding agents) starts with
/// `if common::lacks("name", Capability::Homebrew) { return; }`. It stays
/// meaningful after a platform's switch flips: a supported platform without
/// the capability still skips. Never true on macOS, which declares them all.
pub fn lacks(test: &str, capability: ddc_core::platform::Capability) -> bool {
    use ddc_core::platform;
    match platform::require(capability, platform::CAPABILITIES, None) {
        Ok(()) => false,
        Err(why) => {
            eprintln!("skipped {test}: {why}");
            true
        }
    }
}

/// True, after printing why, when this platform's catalog lists no rule
/// `id` (ADR "Platform seam: one platform module per OS with declared
/// capabilities", §9, amended): a test of a rule only some platforms have,
/// such as Xcode's or the iOS backups', starts with
/// `if common::no_rule("name", "xcode.derived-data") { return; }`. Never
/// true on macOS, whose catalog holds every rule a test names.
pub fn no_rule(test: &str, id: &str) -> bool {
    use ddc_core::platform::PLATFORM;
    if ddc_core::rules::RULES.iter().any(|r| r.id == id) {
        return false;
    }
    eprintln!("skipped {test}: {PLATFORM} lists no rule {id}");
    true
}

/// Whether this platform's catalog lists rule `id`, for a test that adds a
/// platform's own rule to what it checks of the shared ones.
pub fn has_rule(id: &str) -> bool {
    ddc_core::rules::RULES.iter().any(|r| r.id == id)
}

/// Sets the user-immutable flag (`chflags uchg`), which macOS lets the owner
/// set: a delete inside then fails. Elsewhere only root can make a file
/// immutable, so it is `Unsupported` there and the test skips.
pub fn user_immutable(path: &Path) -> std::io::Result<()> {
    use ddc_core::platform::{PLATFORM, Platform};
    if PLATFORM != Platform::Macos {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!("{PLATFORM} has no user-immutable flag"),
        ));
    }
    let ok = std::process::Command::new("/usr/bin/chflags").arg("uchg").arg(path).status()?.success();
    if ok { Ok(()) } else { Err(std::io::Error::other("chflags uchg failed")) }
}

/// Clears what [`user_immutable`] set, under `path`.
pub fn clear_user_immutable(path: &Path) {
    if ddc_core::platform::PLATFORM == ddc_core::platform::Platform::Macos {
        let _ = std::process::Command::new("/usr/bin/chflags").arg("-R").arg("nouchg").arg(path).status();
    }
}

/// The home-relative folders rule `id` names on this platform, in its
/// order: a test of a shared rule builds its fixture where the platform's
/// catalog looks (`Library/Caches/Google/Chrome` on macOS,
/// `.cache/google-chrome` on Linux).
pub fn rule_paths(id: &str) -> &'static [&'static str] {
    use ddc_core::rules::{Matcher, RULES};
    let rule = RULES.iter().find(|r| r.id == id).unwrap_or_else(|| panic!("no rule {id} here"));
    match rule.matcher {
        Matcher::KnownPaths(p) | Matcher::KnownRegion(p) | Matcher::KnownChildren(p) | Matcher::NoTraverse(p) => p,
        _ => panic!("{id} names no folder"),
    }
}

/// [`rule_paths`]'s first folder.
pub fn rule_path(id: &str) -> &'static str {
    rule_paths(id)[0]
}

/// The value of a harness tool (a [`try_symlink`]), or `None` after
/// printing why when the tool is missing here (`io::ErrorKind::Unsupported`),
/// and the test skips (ADR "Platform seam", §9, amended). Any other error
/// fails the test. Never `None` on macOS, where every tool is there.
pub fn skipped_without<T>(test: &str, made: std::io::Result<T>) -> Option<T> {
    match made {
        Ok(value) => Some(value),
        Err(why) if why.kind() == std::io::ErrorKind::Unsupported => {
            eprintln!("skipped {test}: {why}");
            None
        }
        Err(why) => panic!("{test}: {why}"),
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
pub fn symlink(target: impl AsRef<Path>, link: impl AsRef<Path>) {
    let link = link.as_ref();
    try_symlink(target, link).unwrap_or_else(|e| panic!("symlink {}: {e}", link.display()));
}

/// A symlink at `link` to `target`, or `Unsupported` off Unix: the twin a
/// test that does not skip otherwise calls, through [`skipped_without`].
#[cfg(unix)]
pub fn try_symlink(target: impl AsRef<Path>, link: impl AsRef<Path>) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
pub fn try_symlink(_target: impl AsRef<Path>, _link: impl AsRef<Path>) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "the test harness makes symlinks on Unix only"))
}

/// The folders' own blocks, as `du` counts each folder itself: none on
/// APFS, a few KiB each on ext4. A test adds them where a size holds folders
/// the walk read.
pub fn folders<P: AsRef<Path>>(paths: &[P]) -> u64 {
    paths.iter().map(|p| meta::alloc_bytes(&fs::symlink_metadata(p).unwrap())).sum()
}

/// Reference size, as `du -s` counts it: `blocks * 512` of `root`, of every
/// folder and of every other entry under it, hard links counted once,
/// symlinks not followed. Folders add nothing on APFS.
pub fn reference_alloc(root: &Path) -> u64 {
    let mut seen = std::collections::HashSet::new();
    let mut total = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        // Each folder's own blocks, as `du` counts them: none on APFS.
        total += meta::alloc_bytes(&fs::symlink_metadata(&dir).unwrap());
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
