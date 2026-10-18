import Foundation
import Synchronization

/// A stand-in for `deepdrivecleaner`, written to a temporary directory per test.
///
/// Every run appends `start <pid> <args>` and `end <pid> <args>` to a log.
/// Commands (the first argument):
/// - `wait <gate>`: block until the file `<gate>` exists in the gate directory;
/// - `exit <status>`: exit with that status;
/// - `flood`: 1 MiB to stderr, then 1 MiB to stdout, 64-byte lines;
/// - `report <file>`: print the file;
/// - `crash`: kill itself with SIGABRT.
/// Gates replace sleeps, so the tests order runs without timing assumptions.
/// The directory is removed when the fake goes away.
final class FakeCLI {
    let directory: URL
    let executable: URL
    let log: URL

    static let floodLines = 16_384  // 64-byte lines: 1 MiB per stream

    init() throws {
        directory = FileManager.default.temporaryDirectory.appending(path: "ddckit-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        executable = directory.appending(path: "deepdrivecleaner")
        log = directory.appending(path: "runs.log")
        let line63 = { (c: Character) in String(repeating: c, count: 63) }
        let script = """
            #!/bin/sh
            log='\(log.path)'
            gates='\(directory.path)'
            echo "start $$ $*" >> "$log"
            case "$1" in
              wait) while [ ! -e "$gates/$2" ]; do sleep 0.01; done ;;
              exit) echo "end $$ $*" >> "$log"; exit "$2" ;;
              flood) yes \(line63("e")) | head -n \(Self.floodLines) >&2
                     yes \(line63("o")) | head -n \(Self.floodLines) ;;
              report) cat "$2" ;;
              crash) kill -ABRT $$ ;;
            esac
            echo "end $$ $*" >> "$log"
            """
        try script.write(to: executable, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: executable.path)
    }

    deinit {
        try? FileManager.default.removeItem(at: directory)
    }

    func open(_ gate: String) throws {
        try Data().write(to: directory.appending(path: gate))
    }

    /// The log's lines as (event, pid, arguments).
    func runs() throws -> [(event: String, pid: String, args: String)] {
        guard FileManager.default.fileExists(atPath: log.path) else { return [] }
        return try String(contentsOf: log, encoding: .utf8).split(separator: "\n").map { line in
            let parts = line.split(separator: " ", maxSplits: 2).map(String.init)
            return (parts[0], parts[1], parts.count > 2 ? parts[2] : "")
        }
    }

    /// Waits until the log has a line `start … <args>`.
    func waitForStart(_ args: String) async throws {
        for _ in 0..<1_000 {
            if try runs().contains(where: { $0.event == "start" && $0.args == args }) { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        throw CancellationError()
    }
}

/// A counter the runner's `@Sendable` callbacks can share.
final class Counter: Sendable {
    private let value = Mutex(0)
    func increment() { value.withLock { $0 += 1 } }
    var count: Int { value.withLock { $0 } }
}
