//! The freedesktop Trash, ddc's own move (safety ADR §5, amendment of
//! 2026-11-01): the item leaves its verified folder by a rename into a trash
//! folder on its own mount, never by a copy and never across a mount.
//!
//! 1. The home trash (`$XDG_DATA_HOME/Trash`, else `~/.local/share/Trash`)
//!    when the item's folder has its mount id and `dev`, or those of its
//!    nearest existing ancestor.
//! 2. Otherwise the top of the item's mount: `$topdir/.Trash/$uid` when it
//!    exists under a sticky `.Trash` and is the user's, else
//!    `$topdir/.Trash-$uid`, made 0700 or tightened to it.
//! 3. In it, `files/` and `info/`, the user's, never through a symlink.
//! 4. The `.trashinfo` first, then `renameat2(RENAME_NOREPLACE)`, then the
//!    item's `(dev, ino)` checked where it landed.
//!
//! No usable trash, or a rename that would cross a filesystem (`EXDEV`),
//! is [`TrashError::CrossesFilesystem`]: nothing moves, and the item is
//! skipped. The dry run asks [`would_trash`], the same choice with nothing
//! made.

use std::ffi::{OsStr, OsString};
use std::io::{self, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};

use rustix::fs::{Access, AtFlags, Mode, OFlags, RenameFlags};
use rustix::io::Errno;

use super::bulk;
use super::fsops::DirFd;
use crate::platform::types::{TrashError, TrashPlace};

/// Linux's longest name, in bytes.
const NAME_MAX: usize = 255;
const INFO_SUFFIX: &[u8] = b".trashinfo";
/// Names tried in a trash folder before the move gives up.
const TRIES: u32 = 1000;

/// Moves `name`, in the verified folder `parent` (at `path`), to the Trash
/// of its own mount, and tells where it went. `id` is the identity the
/// guard checked; the item must still have it where it lands.
pub fn trash_item(parent: &DirFd, name: &OsStr, path: &Path, id: (u64, u64)) -> Result<Option<TrashPlace>, TrashError> {
    move_into(parent.fd(), name, path, id, &home_trash()?).map(Some)
}

/// The dry run's answer: the same choice of a trash folder as
/// [`trash_item`], with nothing made.
pub fn would_trash(parent: &DirFd, _name: &OsStr, _path: &Path) -> Result<(), TrashError> {
    select(parent.fd(), &home_trash()?, false).map(|_| ())
}

/// `$XDG_DATA_HOME/Trash`; a relative `XDG_DATA_HOME` is ignored, as the
/// specification says.
#[allow(clippy::disallowed_methods)] // XDG_DATA_HOME, read only here
fn home_trash() -> Result<PathBuf, TrashError> {
    let data = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).filter(|d| d.is_absolute());
    match data {
        Some(data) => Ok(data.join("Trash")),
        None => super::folders::home()
            .map(|home| home.join(".local/share/Trash"))
            .ok_or_else(|| other("no home folder for the Trash")),
    }
}

/// The trash folder chosen for an item.
struct Chosen {
    files: OwnedFd,
    info: OwnedFd,
    /// The trash folder itself (`…/Trash`, `$topdir/.Trash-$uid`).
    trash: PathBuf,
    /// The mount top a `Path=` is relative to; `None` for the home trash,
    /// whose `Path=` is absolute.
    top: Option<PathBuf>,
}

/// Steps 1 to 3. With `create`, what is missing is made and the folders
/// are open; without, nothing is made, and `Ok(None)` says the move would
/// find or make one.
fn select(parent: &OwnedFd, home_trash: &Path, create: bool) -> Result<Option<Chosen>, TrashError> {
    let (here, mnt) = bulk::fd_mount(parent).map_err(errno)?;
    let mnt = mnt.ok_or_else(|| other("no mount id for the item's folder"))?;
    if let Some(near) = nearest(home_trash)? {
        let (there, near_mnt) = bulk::fd_mount(&near).map_err(errno)?;
        let near_mnt = near_mnt.ok_or_else(|| other("no mount id for the home trash"))?;
        if near_mnt == mnt && there.0 == here.0 {
            return home(home_trash, here.0, create);
        }
    }
    let top_path = super::statfs::point_by_id(mnt)?.ok_or_else(|| other("the item's mount is not in mountinfo"))?;
    let top = retry(|| {
        rustix::fs::open(
            &top_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
    })
    .map_err(errno)?;
    let (top_id, top_mnt) = bulk::fd_mount(&top).map_err(errno)?;
    if top_mnt != Some(mnt) || bulk::is_mount_root(&top) != Some(true) {
        return Err(other("the top of the item's mount is not where mountinfo says"));
    }
    // A btrfs subvolume reached by path: the mount's top is another
    // device, and a rename there gives `EXDEV`.
    if top_id.0 != here.0 {
        return Err(TrashError::CrossesFilesystem);
    }
    select_top(&top, &top_path, here.0, create)
}

/// The home trash, or its nearest existing ancestor, open without reading
/// it; `None` when not even `/` opens.
fn nearest(home_trash: &Path) -> Result<Option<OwnedFd>, TrashError> {
    let mut at = Some(home_trash);
    while let Some(p) = at {
        match retry(|| rustix::fs::open(p, OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty())) {
            Ok(fd) => return Ok(Some(fd)),
            Err(Errno::NOENT) => at = p.parent(),
            Err(e) => return Err(errno(e)),
        }
    }
    Ok(None)
}

/// Step 2 under an open mount top: `.Trash/$uid`, else `.Trash-$uid`, on
/// the item's device `dev`.
fn select_top(top: &OwnedFd, top_path: &Path, dev: u64, create: bool) -> Result<Option<Chosen>, TrashError> {
    let uid = rustix::process::getuid().as_raw();
    if let Some(shared) = shared(top, uid)? {
        let trash = top_path.join(".Trash").join(uid.to_string());
        return inside(shared, trash, Some(top_path.to_path_buf()), dev, create);
    }
    let own = format!(".Trash-{uid}");
    let trash = top_path.join(&own);
    let fd = match open_dir(top, own.as_str()) {
        Ok(fd) => fd,
        Err(Errno::NOENT) if create => match rustix::fs::mkdirat(top, own.as_str(), Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(Errno::EXIST) => open_dir(top, own.as_str()).map_err(refused)?,
            Err(e) => return Err(refused(e)),
        },
        Err(Errno::NOENT) => {
            return match rustix::fs::accessat(top, ".", Access::WRITE_OK, AtFlags::empty()) {
                Ok(()) => Ok(None),
                Err(e) => Err(refused(e)),
            };
        }
        Err(e) => return Err(refused(e)),
    };
    let st = rustix::fs::fstat(&fd).map_err(errno)?;
    if st.st_uid != uid {
        return Err(TrashError::CrossesFilesystem);
    }
    if st.st_mode & 0o077 != 0 && create {
        // The `trash` crate made them 0755.
        rustix::fs::fchmod(&fd, Mode::from_raw_mode(0o700)).map_err(errno)?;
    }
    inside(fd, trash, Some(top_path.to_path_buf()), dev, create)
}

/// `$topdir/.Trash/$uid`, only when `.Trash` is a real folder with the
/// sticky bit and `$uid` exists, is the user's and is 0700. ddc never makes
/// it: anyone may set the sticky bit on a folder of theirs.
fn shared(top: &OwnedFd, uid: u32) -> Result<Option<OwnedFd>, TrashError> {
    let Ok(dot) = open_dir(top, ".Trash") else { return Ok(None) };
    let st = rustix::fs::fstat(&dot).map_err(errno)?;
    if st.st_mode & 0o1000 == 0 {
        return Ok(None);
    }
    let Ok(mine) = open_dir(&dot, uid.to_string().as_str()) else { return Ok(None) };
    let st = rustix::fs::fstat(&mine).map_err(errno)?;
    Ok((st.st_uid == uid && st.st_mode & 0o777 == 0o700).then_some(mine))
}

/// Step 1: the home trash, made 0700 with its `files/` and `info/` when
/// missing.
fn home(home_trash: &Path, dev: u64, create: bool) -> Result<Option<Chosen>, TrashError> {
    let open = || {
        retry(|| {
            rustix::fs::open(
                home_trash,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
        })
    };
    let fd = match open() {
        Ok(fd) => fd,
        Err(Errno::NOENT) if !create => return Ok(None),
        Err(Errno::NOENT) => {
            if let Some(up) = home_trash.parent() {
                std::fs::create_dir_all(up)?;
            }
            match rustix::fs::mkdir(home_trash, Mode::from_raw_mode(0o700)) {
                Ok(()) | Err(Errno::EXIST) => open().map_err(refused)?,
                Err(e) => return Err(errno(e)),
            }
        }
        Err(e) => return Err(refused(e)),
    };
    let uid = rustix::process::getuid().as_raw();
    if rustix::fs::fstat(&fd).map_err(errno)?.st_uid != uid {
        return Err(TrashError::CrossesFilesystem);
    }
    inside(fd, home_trash.to_path_buf(), None, dev, create)
}

/// Step 3: `files/` and `info/` in the chosen trash, which must be on the
/// item's device `dev`, as `files/` must: a rename gives `EXDEV` otherwise.
fn inside(
    trash_fd: OwnedFd,
    trash: PathBuf,
    top: Option<PathBuf>,
    dev: u64,
    create: bool,
) -> Result<Option<Chosen>, TrashError> {
    let on_dev = |fd: &OwnedFd| bulk::held(fd).map(|held| held.id.0 == dev).map_err(errno);
    if !on_dev(&trash_fd)? {
        return Err(TrashError::CrossesFilesystem);
    }
    let files = sub(&trash_fd, "files", create)?;
    if let Some(files) = &files
        && !on_dev(files)?
    {
        return Err(TrashError::CrossesFilesystem);
    }
    let info = sub(&trash_fd, "info", create)?;
    Ok(match (files, info) {
        (Some(files), Some(info)) if create => Some(Chosen { files, info, trash, top }),
        _ => None,
    })
}

/// One of `files/` and `info/`: the user's, never a symlink; made 0700 when
/// missing and `create`; without `create`, a missing one must be makeable.
fn sub(trash: &OwnedFd, name: &str, create: bool) -> Result<Option<OwnedFd>, TrashError> {
    let fd = match open_dir(trash, name) {
        Ok(fd) => fd,
        Err(Errno::NOENT) if create => match rustix::fs::mkdirat(trash, name, Mode::from_raw_mode(0o700)) {
            Ok(()) | Err(Errno::EXIST) => open_dir(trash, name).map_err(refused)?,
            Err(e) => return Err(refused(e)),
        },
        Err(Errno::NOENT) => {
            return match rustix::fs::accessat(trash, ".", Access::WRITE_OK, AtFlags::empty()) {
                Ok(()) => Ok(None),
                Err(e) => Err(refused(e)),
            };
        }
        Err(e) => return Err(refused(e)),
    };
    if rustix::fs::fstat(&fd).map_err(errno)?.st_uid != rustix::process::getuid().as_raw() {
        return Err(TrashError::CrossesFilesystem);
    }
    Ok(Some(fd))
}

/// Step 4: the info file first, then the rename, then the identity where
/// the item landed. A failure leaves nothing behind but the item where it
/// was.
fn move_into(
    parent: &OwnedFd,
    name: &OsStr,
    path: &Path,
    id: (u64, u64),
    home_trash: &Path,
) -> Result<TrashPlace, TrashError> {
    let chosen = select(parent, home_trash, true)?.ok_or_else(|| other("no trash folder"))?;
    let origin = match &chosen.top {
        Some(top) => path.strip_prefix(top).map_err(|_| other("the item is not under its mount's top"))?,
        None => path,
    };
    let text = info_text(origin, &chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string());
    for n in 0..TRIES {
        let base = trash_name(name, n);
        let mut info_name = base.clone().into_vec();
        info_name.extend_from_slice(INFO_SUFFIX);
        let info_name = OsString::from_vec(info_name);
        let info = match rustix::fs::openat(
            &chosen.info,
            &info_name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        ) {
            Ok(fd) => fd,
            Err(Errno::EXIST) => continue,
            Err(e) => return Err(errno(e)),
        };
        let forget = || {
            let _ = rustix::fs::unlinkat(&chosen.info, &info_name, AtFlags::empty());
        };
        if let Err(e) = std::fs::File::from(info).write_all(text.as_bytes()) {
            forget();
            return Err(e.into());
        }
        match rename_into(parent, name, &chosen.files, &base) {
            Ok(()) => {}
            Err(Errno::EXIST) => {
                forget();
                continue;
            }
            Err(Errno::XDEV) => {
                forget();
                return Err(TrashError::CrossesFilesystem);
            }
            Err(e) => {
                forget();
                return Err(errno(e));
            }
        }
        let landed = bulk::stat_child(&chosen.files, &base);
        if !landed.is_ok_and(|st| (st.dev, st.ino) == id) {
            // Swapped between the check and the rename: put it back.
            if rename_into(&chosen.files, &base, parent, name).is_ok() {
                forget();
                return Err(other("the item changed as it moved, and was put back"));
            }
            // Its info file stays, so a file manager can restore it.
            let at = chosen.trash.join("files").join(&base);
            return Err(TrashError::Other(io::Error::other(format!(
                "the item changed as it moved and could not be put back: it is at {}",
                at.display()
            ))));
        }
        return Ok(TrashPlace { trash: chosen.trash, name: base });
    }
    Err(other("no free name in the trash folder"))
}

/// `from` in `from_dir` renamed to `to` in `to_dir`, never over a name that
/// exists: `renameat2(RENAME_NOREPLACE)`, or where the filesystem refuses
/// the flag (`EINVAL`, as NFS does) a stat of `to`, then `renameat`.
fn rename_into(from_dir: &OwnedFd, from: &OsStr, to_dir: &OwnedFd, to: &OsStr) -> Result<(), Errno> {
    match rustix::fs::renameat_with(from_dir, from, to_dir, to, RenameFlags::NOREPLACE) {
        Err(Errno::INVAL) => {
            if rustix::fs::statat(to_dir, to, AtFlags::SYMLINK_NOFOLLOW).is_ok() {
                return Err(Errno::EXIST);
            }
            rustix::fs::renameat(from_dir, from, to_dir, to)
        }
        done => done,
    }
}

/// The name in `files/`: the item's, then `name.1`, `name.2`…, cut so the
/// info file's name stays within `NAME_MAX`.
fn trash_name(name: &OsStr, n: u32) -> OsString {
    let suffix = if n == 0 { String::new() } else { format!(".{n}") };
    let room = NAME_MAX - INFO_SUFFIX.len() - suffix.len();
    let bytes = name.as_bytes();
    let mut out = bytes[..bytes.len().min(room)].to_vec();
    out.extend_from_slice(suffix.as_bytes());
    OsString::from_vec(out)
}

/// The `.trashinfo` text: `Path=` percent-encoded, `DeletionDate=` in
/// local time.
fn info_text(origin: &Path, date: &str) -> String {
    let mut path = String::new();
    for &b in origin.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            path.push(char::from(b));
        } else {
            path.push_str(&format!("%{b:02X}"));
        }
    }
    format!("[Trash Info]\nPath={path}\nDeletionDate={date}\n")
}

fn open_dir(dir: &OwnedFd, name: &str) -> Result<OwnedFd, Errno> {
    retry(|| {
        rustix::fs::openat(
            dir,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
    })
}

fn retry<T>(mut call: impl FnMut() -> Result<T, Errno>) -> Result<T, Errno> {
    loop {
        match call() {
            Err(Errno::INTR) => continue,
            done => return done,
        }
    }
}

/// What makes a trash folder unusable: a symlink (`ELOOP`), not a folder,
/// not the user's to write (`EACCES`, `EPERM`). A read-only filesystem
/// (`EROFS`) is a failure: `--permanent` cannot remove anything there either.
fn refused(e: Errno) -> TrashError {
    match e {
        Errno::LOOP | Errno::NOTDIR | Errno::ACCESS | Errno::PERM => TrashError::CrossesFilesystem,
        e => errno(e),
    }
}

fn errno(e: Errno) -> TrashError {
    TrashError::Other(io::Error::from_raw_os_error(e.raw_os_error()))
}

fn other(what: &str) -> TrashError {
    TrashError::Other(io::Error::other(what.to_owned()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    use super::*;
    use crate::platform::{meta, testing};

    struct Fx {
        _d: tempfile::TempDir,
        base: PathBuf,
    }

    impl Fx {
        fn new() -> Self {
            let d = tempfile::tempdir().unwrap();
            let base = crate::platform::Canonical::canonical(d.path()).unwrap();
            Self { _d: d, base }
        }
        fn home_trash(&self) -> PathBuf {
            self.base.join("home/.local/share/Trash")
        }
        /// A file `name` in `dir`, its folder open, and its identity.
        fn item(&self, dir: &str, name: &str) -> (OwnedFd, PathBuf, (u64, u64)) {
            let folder = self.base.join(dir);
            fs::create_dir_all(&folder).unwrap();
            let path = folder.join(name);
            fs::write(&path, b"x").unwrap();
            let id = meta::lstat(&path).unwrap().id();
            let fd =
                rustix::fs::open(&folder, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty()).unwrap();
            (fd, path, id)
        }
        fn top(&self) -> OwnedFd {
            open(&self.base)
        }
        fn dev(&self) -> u64 {
            meta::stat(&self.base).unwrap().dev
        }
    }

    fn open(p: &Path) -> OwnedFd {
        rustix::fs::open(p, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC, Mode::empty()).unwrap()
    }

    fn uid() -> u32 {
        rustix::process::getuid().as_raw()
    }

    /// The info file names where the item was, percent-encoded, and when.
    #[test]
    fn the_trashinfo_names_the_origin_encoded_and_the_date() {
        let origin = PathBuf::from(testing::name_from_bytes(b"/a b/100%/\xff").unwrap());
        let text = info_text(&origin, "2026-11-02T10:00:00");
        assert_eq!(text, "[Trash Info]\nPath=/a%20b/100%25/%FF\nDeletionDate=2026-11-02T10:00:00\n");
    }

    /// On the home trash's mount, an item goes to the home trash, which is
    /// made 0700; its info file holds the absolute path; the dry run said so
    /// first and made nothing.
    #[test]
    fn an_item_on_the_homes_mount_moves_to_the_home_trash() {
        let fx = Fx::new();
        let (dir, path, id) = fx.item("work", "cache");
        assert!(select(&dir, &fx.home_trash(), false).unwrap().is_none());
        assert!(!fx.home_trash().exists(), "the dry run makes nothing");

        let place = move_into(&dir, OsStr::new("cache"), &path, id, &fx.home_trash()).unwrap();
        assert_eq!(place, TrashPlace { trash: fx.home_trash(), name: "cache".into() });
        assert!(!path.exists());
        assert_eq!(meta::lstat(&fx.home_trash().join("files/cache")).unwrap().id(), id);
        let info = fs::read_to_string(fx.home_trash().join("info/cache.trashinfo")).unwrap();
        assert!(info.contains(&format!("Path={}\n", path.display())), "{info}");
        assert_eq!(fs::metadata(fx.home_trash()).unwrap().mode() & 0o777, 0o700);
    }

    /// A name taken in the trash gets the next one; a name of 255 bytes is
    /// cut so its info file's name fits.
    #[test]
    fn a_taken_name_gets_the_next_and_a_long_one_is_cut() {
        let fx = Fx::new();
        for _ in 0..2 {
            let (dir, path, id) = fx.item("work", "cache");
            move_into(&dir, OsStr::new("cache"), &path, id, &fx.home_trash()).unwrap();
        }
        assert!(fx.home_trash().join("files/cache.1").exists());
        assert!(fx.home_trash().join("info/cache.1.trashinfo").exists());

        let long = "x".repeat(NAME_MAX);
        let (dir, path, id) = fx.item("work", &long);
        let place = move_into(&dir, OsStr::new(&long), &path, id, &fx.home_trash()).unwrap();
        assert_eq!(place.name.len(), NAME_MAX - INFO_SUFFIX.len());
        assert!(fx.home_trash().join("files").join(&place.name).exists());
    }

    /// An item that is not what the guard checked moves nothing and leaves
    /// no info file: a name that is gone, and an identity that does not
    /// match, put back.
    #[test]
    fn a_failed_move_leaves_the_item_and_no_info_file() {
        let fx = Fx::new();
        let (dir, path, id) = fx.item("work", "cache");
        assert!(move_into(&dir, OsStr::new("gone"), &path, id, &fx.home_trash()).is_err());
        assert!(move_into(&dir, OsStr::new("cache"), &path, (id.0, id.1 ^ 1), &fx.home_trash()).is_err());
        assert!(path.exists(), "put back");
        assert_eq!(crate::stats::read_dir(fx.home_trash().join("info")).unwrap().count(), 0);
        assert_eq!(crate::stats::read_dir(fx.home_trash().join("files")).unwrap().count(), 0);
    }

    /// Under a mount top: a sticky `.Trash` with the user's 0700 folder is
    /// used; a `.Trash` without the sticky bit is not, and `.Trash-$uid` is
    /// made 0700. The dry run makes neither.
    #[test]
    fn the_tops_shared_trash_is_used_only_sticky_with_the_users_folder() {
        let fx = Fx::new();
        assert!(select_top(&fx.top(), &fx.base, fx.dev(), false).unwrap().is_none());
        assert!(!fx.base.join(format!(".Trash-{}", uid())).exists(), "the dry run makes nothing");

        let mine = fx.base.join(".Trash").join(uid().to_string());
        fs::create_dir_all(&mine).unwrap();
        fs::set_permissions(&mine, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(fx.base.join(".Trash"), fs::Permissions::from_mode(0o777)).unwrap();
        let chosen = select_top(&fx.top(), &fx.base, fx.dev(), true).unwrap().unwrap();
        assert_eq!(chosen.trash, fx.base.join(format!(".Trash-{}", uid())), "not sticky");
        assert_eq!(fs::metadata(&chosen.trash).unwrap().mode() & 0o777, 0o700);

        fs::set_permissions(fx.base.join(".Trash"), fs::Permissions::from_mode(0o1777)).unwrap();
        let chosen = select_top(&fx.top(), &fx.base, fx.dev(), true).unwrap().unwrap();
        assert_eq!(chosen.trash, mine);
        assert_eq!(chosen.top.as_deref(), Some(fx.base.as_path()));
    }

    /// A `.Trash-$uid` the user owns with looser bits is tightened to 0700;
    /// the dry run changes nothing.
    #[test]
    fn a_loose_own_trash_is_tightened() {
        let fx = Fx::new();
        let own = fx.base.join(format!(".Trash-{}", uid()));
        fs::create_dir(&own).unwrap();
        fs::set_permissions(&own, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(select_top(&fx.top(), &fx.base, fx.dev(), false).unwrap().is_none());
        assert_eq!(fs::metadata(&own).unwrap().mode() & 0o777, 0o755, "the dry run changes nothing");
        select_top(&fx.top(), &fx.base, fx.dev(), true).unwrap().unwrap();
        assert_eq!(fs::metadata(&own).unwrap().mode() & 0o777, 0o700);
    }

    /// A `.Trash-$uid` that is a symlink, or a `files/` that is one, is no
    /// trash: the clean and the dry run both refuse, as crossing.
    #[test]
    fn a_symlinked_trash_or_files_folder_is_refused_by_both() {
        let fx = Fx::new();
        let elsewhere = fx.base.join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        let own = fx.base.join(format!(".Trash-{}", uid()));
        testing::symlink(&elsewhere, &own).unwrap();
        for create in [false, true] {
            assert!(matches!(select_top(&fx.top(), &fx.base, fx.dev(), create), Err(TrashError::CrossesFilesystem)));
        }
        rustix::fs::unlink(&own).unwrap();

        fs::create_dir(&own).unwrap();
        testing::symlink(&elsewhere, &own.join("files")).unwrap();
        for create in [false, true] {
            assert!(matches!(select_top(&fx.top(), &fx.base, fx.dev(), create), Err(TrashError::CrossesFilesystem)));
        }
        assert_eq!(crate::stats::read_dir(&elsewhere).unwrap().count(), 0);
    }

    /// A top the user cannot write, without a trash of theirs: no trash,
    /// for the clean and the dry run alike. Root writes anyway, so the check
    /// holds for an ordinary user, which the native job is.
    #[test]
    fn a_top_the_user_cannot_write_has_no_trash_for_either() {
        if rustix::process::geteuid().is_root() {
            return;
        }
        let fx = Fx::new();
        let shut = fx.base.join("shut");
        fs::create_dir(&shut).unwrap();
        fs::set_permissions(&shut, fs::Permissions::from_mode(0o500)).unwrap();
        let top = open(&shut);
        let dry = select_top(&top, &shut, fx.dev(), false).map(|_| ());
        let real = select_top(&top, &shut, fx.dev(), true).map(|_| ());
        fs::set_permissions(&shut, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(dry, Err(TrashError::CrossesFilesystem)), "{dry:?}");
        assert!(matches!(real, Err(TrashError::CrossesFilesystem)), "{real:?}");
    }
}
