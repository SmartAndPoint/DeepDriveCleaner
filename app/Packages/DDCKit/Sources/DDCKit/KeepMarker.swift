import Foundation

/// "Protect this folder": an empty `.ddc-keep` in the finding's folder. The
/// scanner turns a folder with a marker into protected user data, and the
/// executor skips it before any rescan (`kept()` in `crates/ddc-core/src/exec/guard.rs`).
public enum KeepMarker {
    public static let name = ".ddc-keep"

    public enum Failure: Error, Equatable {
        case missing
        /// A symlink, a file, or another folder than the scan saw now sits at the path.
        case changed
        case cannotWrite(errno: Int32)
    }

    /// Writes the marker into `path` if it is still the folder the scan saw:
    /// a directory, not a symlink, with the same `(dev, ino)`. A marker that
    /// is already there counts as written.
    public static func protect(path: String, dev: UInt64, ino: UInt64) throws(Failure) {
        var info = stat()
        guard lstat(path, &info) == 0 else { throw errno == ENOENT ? .missing : .cannotWrite(errno: errno) }
        guard info.st_mode & S_IFMT == S_IFDIR, UInt64(info.st_dev) == dev, UInt64(info.st_ino) == ino else {
            throw .changed
        }
        // The folder is opened without following a link, and the marker is
        // created relative to that descriptor: a swap after the check above
        // cannot redirect the write.
        let folder = open(path, O_RDONLY | O_DIRECTORY | O_NOFOLLOW)
        guard folder >= 0 else { throw errno == ELOOP || errno == ENOTDIR ? .changed : .cannotWrite(errno: errno) }
        defer { close(folder) }
        var opened = stat()
        guard fstat(folder, &opened) == 0, UInt64(opened.st_dev) == dev, UInt64(opened.st_ino) == ino else { throw .changed }
        let marker = openat(folder, name, O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW, 0o644)
        if marker >= 0 {
            close(marker)
            return
        }
        guard errno == EEXIST else { throw .cannotWrite(errno: errno) }
    }
}
