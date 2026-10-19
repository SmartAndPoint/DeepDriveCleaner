import Foundation

/// What the app's models need of `CLIRunner`, so tests can give them a fake.
public protocol CLIRunning: Sendable {
    func run(
        _ kind: CLIRunner.Kind,
        _ arguments: [String],
        stderrLine: (@Sendable (String) -> Void)?
    ) async -> CLIRunner.Result
}

extension CLIRunner: CLIRunning {}

/// The one way the app runs the embedded CLI (ADR §1).
///
/// Runs never overlap: the CLI takes a lock, and two of its processes racing for
/// it would make one fail as busy. A scan requested while a scan is queued or
/// running joins it and gets the same result (its own arguments and
/// `stderrLine` are not used); anything else waits its turn, so a clean
/// requested during a scan starts when the scan ends.
public actor CLIRunner {
    public enum Kind: Sendable, Equatable {
        case scan, clean, dryRun, rules
    }

    public struct Result: Sendable {
        public let outcome: RunOutcome
        public let stdout: Data
        /// The last lines of stderr, for the log when a run fails.
        public let stderrTail: [String]

        public init(outcome: RunOutcome, stdout: Data, stderrTail: [String]) {
            self.outcome = outcome
            self.stdout = stdout
            self.stderrTail = stderrTail
        }
    }

    /// stderr lines kept in `Result.stderrTail`.
    static let tailLines = 50

    private let executable: URL
    private var last: Task<Result, Never>?
    private var queuedScan: (id: UUID, task: Task<Result, Never>)?
    private var running: Process?
    private var cancelRequested = false

    public init(executable: URL) {
        self.executable = executable
    }

    /// Runs the CLI with `arguments` after every earlier run has ended.
    /// `stderrLine` receives each stderr line as it arrives (progress).
    public func run(
        _ kind: Kind,
        _ arguments: [String],
        stderrLine: (@Sendable (String) -> Void)? = nil
    ) async -> Result {
        if kind == .scan, let scan = queuedScan {
            return await scan.task.value
        }
        let previous = last
        let id = UUID()
        let task = Task {
            _ = await previous?.value
            let result = await execute(arguments, stderrLine: stderrLine)
            if queuedScan?.id == id { queuedScan = nil }
            return result
        }
        last = task
        if kind == .scan { queuedScan = (id, task) }
        return await task.value
    }

    /// Sends SIGTERM to the running CLI, if any; it then ends as `.cancelled`.
    /// Queued runs still start.
    public func cancel() {
        // `terminate()` on a process that has not started raises an exception.
        guard let running, running.isRunning else { return }
        cancelRequested = true
        running.terminate()
    }

    private func execute(_ arguments: [String], stderrLine: (@Sendable (String) -> Void)?) async -> Result {
        let process = Process()
        process.executableURL = executable
        process.arguments = arguments
        process.standardInput = FileHandle.nullDevice
        let out = Pipe()
        let err = Pipe()
        process.standardOutput = out
        process.standardError = err

        // Both pipes are drained at once: a child that fills one pipe's buffer
        // while the parent waits on the other never finishes.
        let stdout = Task.detached { await Self.readAll(out.fileHandleForReading) }
        let stderr = Task.detached { await Self.readLines(err.fileHandleForReading, each: stderrLine) }

        running = process
        cancelRequested = false
        defer { running = nil }
        let ended: (Int32, Process.TerminationReason)? = await withCheckedContinuation { done in
            process.terminationHandler = { done.resume(returning: ($0.terminationStatus, $0.terminationReason)) }
            do {
                try process.run()
            } catch {
                process.terminationHandler = nil
                done.resume(returning: nil)
            }
        }
        guard let (status, reason) = ended else {
            // Nothing will write: close our write ends so both readers see EOF.
            try? out.fileHandleForWriting.close()
            try? err.fileHandleForWriting.close()
            _ = await (stdout.value, stderr.value)
            return Result(outcome: .notLaunched(reason: "cannot start \(executable.path)"), stdout: Data(), stderrTail: [])
        }
        let outcome = RunOutcome(status: status, reason: reason, cancelRequested: cancelRequested)
        return Result(outcome: outcome, stdout: await stdout.value, stderrTail: await stderr.value)
    }

    // The reads block until the CLI writes or exits, so they run on GCD threads:
    // on Swift's cooperative pool each would hold a thread for the whole run.

    private static func readAll(_ handle: FileHandle) async -> Data {
        await withCheckedContinuation { done in
            DispatchQueue.global(qos: .utility).async {
                var data = Data()
                while case let chunk = handle.availableData, !chunk.isEmpty { data.append(chunk) }
                done.resume(returning: data)
            }
        }
    }

    /// Calls `each` per line as it arrives and returns the last `tailLines` lines.
    private static func readLines(_ handle: FileHandle, each: (@Sendable (String) -> Void)?) async -> [String] {
        await withCheckedContinuation { done in
            DispatchQueue.global(qos: .utility).async {
                var tail: [String] = []
                var pending = Data()
                func emit(_ bytes: Data) {
                    let line = String(decoding: bytes, as: UTF8.self)
                    each?(line)
                    tail.append(line)
                    if tail.count > tailLines { tail.removeFirst() }
                }
                while case let chunk = handle.availableData, !chunk.isEmpty {
                    pending.append(chunk)
                    while let newline = pending.firstIndex(of: 0x0A) {
                        emit(pending[pending.startIndex..<newline])
                        pending = pending[(newline + 1)...]
                    }
                }
                if !pending.isEmpty { emit(pending) }
                done.resume(returning: tail)
            }
        }
    }
}
