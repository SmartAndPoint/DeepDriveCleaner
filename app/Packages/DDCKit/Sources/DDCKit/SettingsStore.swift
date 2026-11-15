import Foundation
import Observation

/// The app's one writer of `settings.json` (story "Settings: folders, types,
/// notifications"; menu bar ADR §6 item 6).
///
/// Every edit takes `<data dir>/settings.lock`, reads the file from disk
/// again, applies itself and writes the whole file back atomically, keeping
/// its mode, so a key it did not edit survives, the `scheduler` record and
/// `agents_clean_safe` included. The CLI holds the same lock for its writes
/// (`clear_scheduler`, `ddc settings set`; menu bar ADR §6 item 6, amendment
/// of 2026-11-15), so neither loses the other's change: an `off` the CLI
/// wrote is never undone by an edit that read the file before it. Rust's
/// `File::try_lock` is `flock(2)` on macOS too, so the two exclude each
/// other. The lock is held only inside `update`, never across a CLI run.
///
/// A file the app cannot read, malformed or of a newer version, is never
/// overwritten: `problem` says why, and the panes show it.
@MainActor
@Observable
public final class SettingsStore {
    /// Rust's `settings::SETTINGS_LOCK`, beside `settings.json`.
    nonisolated public static let lockName = "settings.lock"
    /// How long an edit tries for the lock before it gives up, as the CLI.
    static let lockWait: Duration = .seconds(1)

    /// The file as last read or written; the defaults while it cannot be read.
    public private(set) var file = SettingsFile()
    /// Why Settings cannot change the file; nil when it can.
    public private(set) var problem: String?

    @ObservationIgnored private let url: URL
    @ObservationIgnored private let changed: @MainActor () -> Void
    @ObservationIgnored private let log: @MainActor (String) -> Void

    /// `changed` runs after each write: the app refreshes the popover, so
    /// ignored findings and the icon change at once, with no scan.
    public init(
        url: URL,
        changed: @escaping @MainActor () -> Void = {},
        log: @escaping @MainActor (String) -> Void = { _ in }
    ) {
        self.url = url
        self.changed = changed
        self.log = log
        reload()
    }

    /// Reads the file again: when Settings opens, and before every edit.
    @discardableResult
    public func reload() -> Bool {
        do {
            file = try SettingsFile.read(at: url)
            problem = nil
            return true
        } catch {
            problem = Self.describe(error)
            return false
        }
    }

    /// Applies `edit` to the file as it is on disk now and writes it back.
    /// False when the file cannot be read or written; `problem` says why.
    @discardableResult
    public func update(_ edit: (inout SettingsFile) -> Void) -> Bool {
        let lock: Int32
        do {
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            lock = try takeLock()
        } catch {
            problem = "settings.json could not be written (\(error.localizedDescription))."
            log("settings.json not written: \(error)")
            return false
        }
        defer { if lock >= 0 { close(lock) } }
        guard reload() else {
            log("settings.json not written: \(problem ?? "")")
            return false
        }
        var edited = file
        edit(&edited)
        guard edited != file else { return true }
        do {
            let mode = (try? FileManager.default.attributesOfItem(atPath: url.path))?[.posixPermissions]
            try edited.encoded().write(to: url, options: .atomic)
            if let mode {
                try FileManager.default.setAttributes([.posixPermissions: mode], ofItemAtPath: url.path)
            }
        } catch {
            problem = "settings.json could not be written (\(error.localizedDescription))."
            log("settings.json write failed: \(error)")
            return false
        }
        file = edited
        changed()
        return true
    }

    enum LockError: LocalizedError {
        case busy
        case failed(Int32)

        var errorDescription: String? {
            switch self {
            case .busy: "another program is changing it; try again"
            case .failed(let code): String(cString: strerror(code))
            }
        }
    }

    /// `settings.lock`, held: its descriptor, which closing releases; -1
    /// where the file system takes no locks, and the edit goes on without
    /// one, as every writer did before. Tries for `lockWait`, then `busy`.
    private func takeLock() throws -> Int32 {
        let path = url.deletingLastPathComponent().appending(path: Self.lockName).path
        let fd = open(path, O_RDWR | O_CREAT | O_CLOEXEC, 0o644)
        guard fd >= 0 else { throw LockError.failed(errno) }
        let clock = ContinuousClock()
        let started = clock.now
        while flock(fd, LOCK_EX | LOCK_NB) != 0 {
            let code = errno
            if code == ENOTSUP || code == EOPNOTSUPP || code == ENOLCK {
                log("settings.lock: the file system takes no locks; writing without one")
                close(fd)
                return -1
            }
            guard code == EWOULDBLOCK, clock.now - started < Self.lockWait else {
                close(fd)
                throw code == EWOULDBLOCK ? LockError.busy : LockError.failed(code)
            }
            usleep(20_000)
        }
        return fd
    }

    static func describe(_ error: SharedSettings.ReadError) -> String {
        switch error {
        case .malformed(_, let reason):
            "settings.json could not be read (\(reason)), so Settings leaves it as it is. Fix or remove the file to change these settings here."
        case .otherSchema(let version):
            "settings.json is of another version (\(version)), written by another DeepDriveCleaner, so Settings leaves it as it is."
        }
    }
}
