import Foundation

/// Where the CLI keeps `last-scan.json` and `settings.json`, found the way
/// `Env::from_process` finds it: `DDC_DATA_DIR` when set and not empty, else
/// `~/Library/Application Support/DeepDriveCleaner`. The app never sets the
/// variable; it inherits it, as the CLI it starts does.
public struct DataDirectory: Sendable, Equatable {
    public let url: URL
    /// Where the CLI writes `actions.jsonl`: `<data>/logs` when the data
    /// directory is given, else `~/Library/Logs/DeepDriveCleaner`.
    public let logs: URL

    /// `logs` defaults to `<url>/logs`, as with `DDC_DATA_DIR`.
    public init(url: URL, logs: URL? = nil) {
        self.url = url
        self.logs = logs ?? url.appending(path: "logs", directoryHint: .isDirectory)
    }

    public static func current(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        home: URL = FileManager.default.homeDirectoryForCurrentUser
    ) -> DataDirectory {
        if let dir = environment["DDC_DATA_DIR"], !dir.isEmpty {
            return DataDirectory(url: URL(filePath: dir, directoryHint: .isDirectory))
        }
        return DataDirectory(
            url: home.appending(path: "Library/Application Support/DeepDriveCleaner", directoryHint: .isDirectory),
            logs: home.appending(path: "Library/Logs/DeepDriveCleaner", directoryHint: .isDirectory)
        )
    }

    /// The action journal every clean records each item in.
    public var journal: URL { logs.appending(path: "actions.jsonl") }

    public var lastScan: URL { url.appending(path: "last-scan.json") }
    public var settings: URL { url.appending(path: "settings.json") }

    /// The file's modification date, or nil when it does not exist.
    public static func modified(_ file: URL) -> Date? {
        (try? FileManager.default.attributesOfItem(atPath: file.path))?[.modificationDate] as? Date
    }
}
