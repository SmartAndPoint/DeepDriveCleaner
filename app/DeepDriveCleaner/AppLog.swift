import Foundation

/// `~/Library/Logs/DeepDriveCleaner/app.log`, next to the CLI's logs.
///
/// The app is `LSUIElement`, so its stdout is invisible; this file is the one
/// channel (GrammarHelper's FileLogger). Each line carries the process id, so
/// a scheduled instance and a user's instance can be told apart.
enum AppLog {
    static let url = FileManager.default.homeDirectoryForCurrentUser
        .appending(path: "Library/Logs/DeepDriveCleaner/app.log")

    private static let queue = DispatchQueue(label: "com.smartandpoint.deepdrivecleaner.applog")
    private static let stamp = Date.ISO8601FormatStyle(includingFractionalSeconds: true, timeZone: .current)
    private static let pid = ProcessInfo.processInfo.processIdentifier

    static func write(_ message: String) {
        let line = "\(Date.now.formatted(stamp)) [\(pid)] \(message)\n"
        queue.async {
            let files = FileManager.default
            if !files.fileExists(atPath: url.path) {
                try? files.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
                files.createFile(atPath: url.path, contents: nil)
            }
            guard let handle = try? FileHandle(forWritingTo: url) else { return }
            defer { try? handle.close() }
            _ = try? handle.seekToEnd()
            try? handle.write(contentsOf: Data(line.utf8))
        }
    }

    /// Waits for every queued line; call before `exit`.
    static func flush() {
        queue.sync {}
    }
}
