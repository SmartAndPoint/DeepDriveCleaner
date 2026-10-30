import Foundation

/// What the app's models need of `CLIRunner`, so tests can give them a fake.
public protocol CLIRunning: Sendable {
    func run(_ kind: CLIRunner.Kind, _ arguments: [String], options: CLIRunner.Options) async -> CLIRunner.Result
    /// Stops the run of `ticket` only: SIGTERM if it is running, and never
    /// started if it is still waiting.
    func cancel(_ ticket: CLIRunner.Ticket) async
}

extension CLIRunning {
    public func run(
        _ kind: CLIRunner.Kind,
        _ arguments: [String],
        stderrLine: (@Sendable (String) -> Void)? = nil
    ) async -> CLIRunner.Result {
        await run(kind, arguments, options: CLIRunner.Options(stderrLine: stderrLine))
    }
}

extension CLIRunner: CLIRunning {}

/// The one way the app runs the embedded CLI (ADR §1).
///
/// Every run but the lane kinds (`.disk`, `.diskAgents`, `.brewPackages`) goes through one queue and never overlaps another:
/// two that take the ddc lock would make one fail as busy, and `rules`,
/// `notify` and `schedule` are queued too. A scan requested while a scan is queued
/// or running joins it and gets the same result (its own arguments and
/// `stderrLine` are not used); anything else waits its turn, so a clean
/// requested during a scan starts when the scan ends.
///
/// `.disk`, `.diskAgents` and `.brewPackages` take no lock, and the app keeps
/// what they write apart from any reader, so each runs in a lane of its own, beside the
/// queue: it never waits for a run of the queue or of another lane, and
/// nothing waits for it. A lane holds one run. A second request with the
/// same arguments joins it; one with other arguments waits for it, then
/// runs (ADR §1, amended 2026-10-28, stories "Whole disk in the app",
/// "Coding agents in the Whole disk view" and "Whole disk: Homebrew
/// packages, their sizes, and brew uninstall"). A `brew-packages` listing
/// may run beside a `disk free` that runs brew: an accepted race.
public actor CLIRunner {
    /// `.scan` and the lanes coalesce: a scheduled `.backgroundScan` never hands
    /// a caller another run's output, nor do `notify` and `schedule`.
    public enum Kind: Sendable, Equatable, Hashable {
        case scan, clean, dryRun, rules
        /// `scan --background --notify json`, the scheduled scan.
        case backgroundScan
        /// `notify ack` and `notify snooze`.
        case notify
        /// `schedule uninstall`, in the migration from the 0.1.0 agent.
        case schedule
        /// `disk --json`, the whole-disk report: read-only, outside the queue.
        case disk
        /// `disk agents --json`, the Coding agents section: read-only, in a
        /// lane of its own.
        case diskAgents
        /// `disk free --actions <file> --json`: takes the exclusive lock, so
        /// it waits its turn in the queue, and a scan behind it waits for it.
        case diskFree
        /// `disk free --actions <file> --dry-run --json`: the shared lock.
        case diskFreeDryRun

        /// `disk brew-packages --json`, the Homebrew packages sheet's list:
        /// read-only and lock-free, in a lane of its own (menu bar ADR §6
        /// item 10, amended 2026-10-29).
        case brewPackages

        /// `keep remove <folder> --folder-dev N --folder-ino N --json`,
        /// Remove Protection… (menu bar ADR §6 item 12, amended 2026-10-30):
        /// takes the exclusive lock, so it waits its turn in the queue.
        case keepRemove

        /// Runs in a lane of its own, beside the queue.
        public var hasLane: Bool { self == .disk || self == .diskAgents || self == .brewPackages }
    }

    /// Names one run, so it can be cancelled without touching another.
    public struct Ticket: Hashable, Sendable {
        let id = UUID()
        public init() {}
    }

    public struct Options: Sendable {
        public var ticket: Ticket?
        /// Each stdout line as it arrives; `Result.stdout` still holds all of it.
        public var stdoutLine: (@Sendable (String) -> Void)?
        /// Each stderr line as it arrives, on the reader's thread: it must
        /// not block, or the CLI stalls on a full pipe.
        public var stderrLine: (@Sendable (String) -> Void)?
        /// Called when the process has started: for a run of the queue, after
        /// every earlier run of the queue ended.
        public var onStart: (@Sendable () -> Void)?

        public init(
            ticket: Ticket? = nil,
            stdoutLine: (@Sendable (String) -> Void)? = nil,
            stderrLine: (@Sendable (String) -> Void)? = nil,
            onStart: (@Sendable () -> Void)? = nil
        ) {
            self.ticket = ticket
            self.stdoutLine = stdoutLine
            self.stderrLine = stderrLine
            self.onStart = onStart
        }
    }

    public struct Result: Sendable {
        public let outcome: RunOutcome
        public let stdout: Data
        /// The last lines of stderr, for the log when a run fails; progress
        /// events (lines starting with `{`) are left out.
        public let stderrTail: [String]

        public init(outcome: RunOutcome, stdout: Data, stderrTail: [String]) {
            self.outcome = outcome
            self.stdout = stdout
            self.stderrTail = stderrTail
        }
    }

    /// stderr lines kept in `Result.stderrTail`.
    static let tailLines = 50

    /// A started process, keyed in `running` by its run.
    private struct Running {
        let process: Process
        let ticket: Ticket?
        var cancelRequested = false
    }

    private let executable: URL
    private var last: Task<Result, Never>?
    private var queuedScan: (id: UUID, task: Task<Result, Never>)?
    private var lanes: [Kind: (id: UUID, arguments: [String], task: Task<Result, Never>)] = [:]
    /// One entry per process running: the queue's and the lanes'.
    private var running: [UUID: Running] = [:]
    private var cancelled: Set<Ticket> = []

    public init(executable: URL) {
        self.executable = executable
    }

    /// Runs the CLI with `arguments` after every earlier run of the queue has
    /// ended; a kind with a lane at once, or after its lane's run.
    public func run(_ kind: Kind, _ arguments: [String], options: Options) async -> Result {
        if kind.hasLane { return await runInLane(kind, arguments, options: options) }
        if kind == .scan, let scan = queuedScan {
            return await scan.task.value
        }
        let previous = last
        let id = UUID()
        let task = Task {
            _ = await previous?.value
            let result = await startUnlessCancelled(id: id, arguments, options: options)
            if queuedScan?.id == id { queuedScan = nil }
            return result
        }
        last = task
        if kind == .scan { queuedScan = (id, task) }
        return await task.value
    }

    /// A lane: never reads or sets `last`, so it neither waits for the queue
    /// nor holds it up. A request for other arguments never receives the
    /// running one's output (`--no-sizes` is not the sizes report).
    private func runInLane(_ kind: Kind, _ arguments: [String], options: Options) async -> Result {
        while let lane = lanes[kind] {
            if lane.arguments == arguments { return await lane.task.value }
            _ = await lane.task.value
        }
        let id = UUID()
        let task = Task {
            let result = await startUnlessCancelled(id: id, arguments, options: options)
            if lanes[kind]?.id == id { lanes[kind] = nil }
            return result
        }
        lanes[kind] = (id, arguments, task)
        return await task.value
    }

    private func startUnlessCancelled(id: UUID, _ arguments: [String], options: Options) async -> Result {
        if let ticket = options.ticket, cancelled.remove(ticket) != nil {
            // Cancelled while it waited: it never starts.
            return Result(outcome: .cancelled, stdout: Data(), stderrTail: [])
        }
        return await execute(id: id, arguments, options: options)
    }

    /// SIGTERM to the run of `ticket` if it is running (it then ends as
    /// `.cancelled`); if it is still waiting, it never starts. No other run
    /// is touched.
    public func cancel(_ ticket: Ticket) {
        if let (id, run) = running.first(where: { $0.value.ticket == ticket }) {
            // `terminate()` on a process that has not started raises an exception.
            guard run.process.isRunning else { return }
            running[id]?.cancelRequested = true
            run.process.terminate()
        } else {
            // Still waiting: it never starts. (A ticket whose run is over stays unused.)
            cancelled.insert(ticket)
        }
    }

    private func execute(id: UUID, _ arguments: [String], options: Options) async -> Result {
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
        let stdout = Task.detached { await Self.readAll(out.fileHandleForReading, each: options.stdoutLine) }
        let stderr = Task.detached { await Self.readLines(err.fileHandleForReading, each: options.stderrLine) }

        running[id] = Running(process: process, ticket: options.ticket)
        defer {
            running[id] = nil
            if let ticket = options.ticket { cancelled.remove(ticket) }
        }
        let ended: (Int32, Process.TerminationReason)? = await withCheckedContinuation { done in
            process.terminationHandler = { done.resume(returning: ($0.terminationStatus, $0.terminationReason)) }
            do {
                try process.run()
                options.onStart?()
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
        let outcome = RunOutcome(status: status, reason: reason, cancelRequested: running[id]?.cancelRequested ?? false)
        return Result(outcome: outcome, stdout: await stdout.value, stderrTail: await stderr.value)
    }

    // The reads block until the CLI writes or exits, so they run on GCD threads:
    // on Swift's cooperative pool each would hold a thread for the whole run.

    /// All of stdout, and each line to `each` as it arrives (the dry run's predictions).
    private static func readAll(_ handle: FileHandle, each: (@Sendable (String) -> Void)?) async -> Data {
        await withCheckedContinuation { done in
            DispatchQueue.global(qos: .utility).async {
                var data = Data()
                var lineStart = 0
                while case let chunk = handle.availableData, !chunk.isEmpty {
                    data.append(chunk)
                    guard let each else { continue }
                    while let newline = data[lineStart...].firstIndex(of: 0x0A) {
                        each(String(decoding: data[lineStart..<newline], as: UTF8.self))
                        lineStart = newline + 1
                    }
                }
                if let each, lineStart < data.count { each(String(decoding: data[lineStart...], as: UTF8.self)) }
                done.resume(returning: data)
            }
        }
    }

    /// Calls `each` per line as it arrives and returns the last `tailLines`
    /// lines that are not progress events.
    private static func readLines(_ handle: FileHandle, each: (@Sendable (String) -> Void)?) async -> [String] {
        await withCheckedContinuation { done in
            DispatchQueue.global(qos: .utility).async {
                var tail: [String] = []
                var pending = Data()
                func emit(_ bytes: Data) {
                    let line = String(decoding: bytes, as: UTF8.self)
                    each?(line)
                    guard !line.hasPrefix("{") else { return }
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
