import Foundation

/// One holder of the CLI's data-dir lock, as `deepdrivecleaner lock status
/// --json` lists it and the busy envelope on exit 75 carries it (story
/// "Busy: name the scan or clean that holds the lock, and offer to stop it";
/// menu bar ADR §6 item 16; schema `docs/lock.schema.json`). Strict: an
/// unknown value of an enum is a decoding error.
public struct LockHolder: Decodable, Sendable, Equatable, Hashable, Identifiable {
    /// What took the lock. The case names avoid a `.clean` token, which only
    /// the clean's own files may hold (`ContractTests.cleanHasOneOwner`).
    public enum Command: String, Decodable, Sendable, CaseIterable {
        case scan
        case backgroundScan = "background-scan"
        case wholeClean = "clean"
        case wholeCleanDryRun = "clean-dry-run"
        case itemsClean = "clean-items"
        case itemsCleanDryRun = "clean-items-dry-run"
        case toolCleanup = "disk-free"
        case toolDryRun = "disk-free-dry-run"
        case keepRemoval = "keep-remove"
    }

    public enum Mode: String, Decodable, Sendable, CaseIterable {
        case shared, exclusive
    }

    /// Where it was started: a guess, for the words only.
    public enum Origin: String, Decodable, Sendable, CaseIterable {
        case app, terminal, other
    }

    /// A clean's phase: what Stop does to it.
    public enum Phase: String, Decodable, Sendable, CaseIterable {
        case preparing, cleaning
    }

    public struct Progress: Decodable, Sendable, Equatable, Hashable {
        public let done: UInt64
        public let total: UInt64
    }

    /// The holder itself, whatever its progress: a stopping set keys on it.
    public struct ID: Sendable, Hashable {
        public let pid: Int32
        public let startUs: UInt64
    }

    public var id: ID { ID(pid: pid, startUs: startUs) }

    public let pid: Int32
    /// The start in microseconds since the epoch: what `lock stop --started` takes.
    public let startUs: UInt64
    public let started: String
    public let command: Command
    public let mode: Mode
    public let origin: Origin
    public let phase: Phase?
    public let progress: Progress?
}

/// What `deepdrivecleaner lock status --json` prints.
public struct LockStatus: Decodable, Sendable, Equatable {
    /// `LOCK_SCHEMA_VERSION` of the CLI this app embeds.
    public static let schemaVersion = 1

    public let holders: [LockHolder]

    public static func read(_ data: Data) throws -> LockStatus {
        let decoder = JSONDecoder.snakeCase
        let version = try decoder.decode(VersionHeader.self, from: data).schemaVersion
        guard version == schemaVersion else { throw ScanReport.ReadError.otherSchema(version) }
        return try decoder.decode(LockStatus.self, from: data)
    }
}

/// The error envelope a command prints on stdout under `--json` when it
/// meets a held lock (exit 75; platform seam ADR §7, amended 2026-11-08).
public struct BusyReport: Sendable, Equatable {
    /// `ERROR_SCHEMA_VERSION` of the CLI this app embeds.
    public static let errorSchemaVersion = 3

    /// The live holders whose records were found; empty when none wrote one.
    public let holders: [LockHolder]

    public enum ReadError: Error, Equatable {
        case otherSchema(Int)
        case notBusy(String)
    }

    private struct Envelope: Decodable {
        let errorSchemaVersion: Int
        let error: Body
    }

    private struct Body: Decodable {
        let code: String
        let holders: [LockHolder]?
    }

    private struct Header: Decodable {
        let errorSchemaVersion: Int
    }

    /// The holders the envelope in `stdout` names; none when it names none
    /// or cannot be read.
    public static func holders(in stdout: Data) -> [LockHolder] {
        (try? read(stdout))?.holders ?? []
    }

    public static func read(_ data: Data) throws -> BusyReport {
        let decoder = JSONDecoder.snakeCase
        let version = try decoder.decode(Header.self, from: data).errorSchemaVersion
        guard version == errorSchemaVersion else { throw ReadError.otherSchema(version) }
        let envelope = try decoder.decode(Envelope.self, from: data)
        guard envelope.error.code == "busy" else { throw ReadError.notBusy(envelope.error.code) }
        return BusyReport(holders: envelope.error.holders ?? [])
    }
}

/// What the app wants to run when it meets the lock: it decides which
/// holders block it.
public enum LockPurpose: Sendable, Equatable {
    /// The sheet's Clean: it needs the lock alone, so any holder blocks it.
    case cleaning
    /// The sheet's check, a dry run of the list: it shares the lock.
    case checking
    /// Scan now: it shares the lock.
    case scanning

    /// Whether `holders` keep this run from starting.
    public func blocked(by holders: [LockHolder]) -> Bool {
        switch self {
        case .cleaning: !holders.isEmpty
        case .checking, .scanning: holders.contains { $0.mode == .exclusive }
        }
    }
}

extension Wording {
    /// "A clean started from a terminal 3 min ago (pid 4101), on item 4 of 8".
    public static func lockHolder(_ h: LockHolder, now: Date) -> String {
        let what = switch h.command {
        case .scan: "A scan"
        case .backgroundScan: "A scheduled scan"
        case .wholeClean, .itemsClean: "A clean"
        case .wholeCleanDryRun, .itemsCleanDryRun: "A clean's check"
        case .toolCleanup: "A tool's cleanup (disk free)"
        case .toolDryRun: "A tool's dry run (disk free)"
        case .keepRemoval: "Remove Protection"
        }
        let from = switch h.origin {
        case .terminal: " from a terminal"
        case .app: " from DeepDriveCleaner"
        case .other: ""
        }
        let when = date(rfc3339: h.started).map { " " + age(of: $0, now: now) } ?? ""
        var line = "\(what) started\(from)\(when) (pid \(h.pid))"
        switch (h.phase, h.progress) {
        case let (.cleaning?, progress?) where progress.done < progress.total:
            line += ", on item \(progress.done + 1) of \(progress.total)"
        case (.cleaning?, .some): line += ", finishing"
        case (.cleaning?, nil): line += ", starting its first item"
        case (.preparing?, _): line += ", re-checking its items"
        case (nil, _): break
        }
        return line
    }

    /// The busy panel's first line when no holder is named.
    public static let lockNoHolder = "Another scan or clean holds the lock, but it does not say which."

    /// Shown in place of what Stop does once a Stop was sent.
    public static func lockStopping(_ h: LockHolder) -> String {
        h.phase == .cleaning ? "Stopping: finishing the current item…" : "Stopping…"
    }

    /// The busy panel's line for a Clean that met the lock: it did not start.
    public static let lockCleanNotStarted = "The clean did not start, and nothing was touched."

    /// Beside Wait: what it will do.
    public static func lockWaitHint(_ purpose: LockPurpose) -> String {
        switch purpose {
        case .cleaning: "Clean these items as soon as the lock is free, while this sheet stays open."
        case .checking: "Check again as soon as the lock is free."
        case .scanning: "Scan as soon as the lock is free."
        }
    }

    /// While Wait is on.
    public static func lockWaiting(_ purpose: LockPurpose) -> String {
        switch purpose {
        case .cleaning: "Waiting to clean…"
        case .checking: "Waiting to check…"
        case .scanning: "Waiting to scan…"
        }
    }

    /// What Stop does to `h`, said before Stop.
    public static func lockStopEffect(_ h: LockHolder) -> String {
        switch (h.command, h.phase) {
        case (_, .cleaning?): "Stop ends it after this item."
        case (_, .preparing?): "Stop ends it at once, before it removes anything."
        case (.toolCleanup, _): "Stop ends it before its next action; a tool's command is interrupted."
        case (.toolDryRun, _): "Stop ends it after its current check."
        default: "Stop ends it at once."
        }
    }
}

/// The busy state of a run the app could not start (the sheet's check, its
/// Clean, the popover's Scan now): who holds the lock, whether Wait is on,
/// and which holders were sent a Stop.
public struct LockBusy: Sendable, Equatable {
    public let purpose: LockPurpose
    public internal(set) var holders: [LockHolder]
    public internal(set) var waiting = false
    public internal(set) var stopping: Set<LockHolder.ID> = []

    public init(purpose: LockPurpose, holders: [LockHolder]) {
        self.purpose = purpose
        self.holders = holders
    }

    /// The holders read again: a Stop stays shown only while its holder is.
    mutating func update(_ holders: [LockHolder]) {
        self.holders = holders
        stopping.formIntersection(holders.map(\.id))
    }
}

/// Reads and stops the lock's holders through the CLI (never the records
/// themselves; ADR §1), and paces a Wait.
public struct LockHolders: Sendable {
    /// How often a Wait reads `lock status` while a named holder blocks.
    public static let pollInterval: Duration = .seconds(3)
    /// How long a Wait waits before it tries a run again with no holder
    /// named, or after a run it started met the lock again.
    public static let retryInterval: Duration = .seconds(15)

    let runner: any CLIRunning
    let sleep: @Sendable (Duration) async throws -> Void

    public init(runner: any CLIRunning, sleep: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) }) {
        self.runner = runner
        self.sleep = sleep
    }

    /// The live holders now; `nil` when `lock status` failed or printed
    /// something unreadable.
    public func status() async -> [LockHolder]? {
        let result = await runner.run(.lockStatus, ["lock", "status", "--json"])
        guard result.outcome == .done else { return nil }
        return try? LockStatus.read(result.stdout).holders
    }

    /// Sends `h` an interrupt through `lock stop`; true when the CLI sent
    /// it. A refusal (the holder changed) is false: the caller reads
    /// `status()` again.
    public func stop(_ h: LockHolder) async -> Bool {
        let result = await runner.run(.lockStop, ["lock", "stop", "--pid", String(h.pid), "--started", String(h.startUs)])
        return result.outcome == .done
    }

    /// Waits until nothing blocks `purpose`: polls `lock status` while a
    /// holder is named, else waits one retry interval. `again`: a run this
    /// Wait started met the lock again, so it first waits one retry
    /// interval whatever `lock status` says (story: no sooner than 15 s).
    /// Returns false when the task was cancelled (the sheet closed, Stop
    /// Waiting), also during a `lock status` run; true when the run should
    /// be tried.
    public func waitUntilFree(
        for purpose: LockPurpose,
        named: Bool,
        again: Bool = false,
        onHolders: (@Sendable ([LockHolder]) async -> Void)? = nil
    ) async -> Bool {
        if again || !named {
            guard (try? await sleep(Self.retryInterval)) != nil, !Task.isCancelled else { return false }
            if !named { return true }
        }
        while !Task.isCancelled {
            guard let holders = await status() else {
                return (try? await sleep(Self.retryInterval)) != nil && !Task.isCancelled
            }
            await onHolders?(holders)
            if !purpose.blocked(by: holders) { return !Task.isCancelled }
            guard (try? await sleep(Self.pollInterval)) != nil else { return false }
        }
        return false
    }
}
