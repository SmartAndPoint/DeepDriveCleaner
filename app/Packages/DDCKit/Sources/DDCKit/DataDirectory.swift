import Foundation

/// Where the CLI keeps `last-scan.json` and `settings.json`, found the way
/// `Env::from_process` finds it: `DDC_DATA_DIR` when set and not empty, else
/// `~/Library/Application Support/DeepDriveCleaner`. The app never sets the
/// variable; it inherits it, as the CLI it starts does.
public struct DataDirectory: Sendable, Equatable {
    public let url: URL

    public init(url: URL) {
        self.url = url
    }

    public static func current(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        home: URL = FileManager.default.homeDirectoryForCurrentUser
    ) -> DataDirectory {
        if let dir = environment["DDC_DATA_DIR"], !dir.isEmpty {
            return DataDirectory(url: URL(filePath: dir, directoryHint: .isDirectory))
        }
        return DataDirectory(url: home.appending(path: "Library/Application Support/DeepDriveCleaner", directoryHint: .isDirectory))
    }

    public var lastScan: URL { url.appending(path: "last-scan.json") }
    public var settings: URL { url.appending(path: "settings.json") }

    /// The file's modification date, or nil when it does not exist.
    public static func modified(_ file: URL) -> Date? {
        (try? FileManager.default.attributesOfItem(atPath: file.path))?[.modificationDate] as? Date
    }
}
