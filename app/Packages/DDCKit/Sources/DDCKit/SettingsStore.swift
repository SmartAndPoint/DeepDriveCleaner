import Foundation
import Observation

/// The app's one writer of `settings.json` (story "Settings: folders, types,
/// notifications"; menu bar ADR §6 item 6).
///
/// Every edit reads the file from disk again, applies itself and writes the
/// whole file back atomically, so a key it did not edit survives, the
/// `scheduler` record included. The CLI's `clear_scheduler` does the same
/// without a lock; both are atomic renames, so the worst case is one lost
/// `scheduler` clear, which the next launch heals.
///
/// A file the app cannot read, malformed or of a newer version, is never
/// overwritten: `problem` says why, and the panes show it.
@MainActor
@Observable
public final class SettingsStore {
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
        guard reload() else {
            log("settings.json not written: \(problem ?? "")")
            return false
        }
        var edited = file
        edit(&edited)
        guard edited != file else { return true }
        do {
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try edited.encoded().write(to: url, options: .atomic)
        } catch {
            problem = "settings.json could not be written (\(error.localizedDescription))."
            log("settings.json write failed: \(error)")
            return false
        }
        file = edited
        changed()
        return true
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
