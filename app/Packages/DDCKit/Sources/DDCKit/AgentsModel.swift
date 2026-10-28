import Foundation
import Observation

/// The Coding agents section's state (story "Coding agents in the Whole disk
/// view"): `ddc disk agents --json --no-sizes`, whose sessions show at once,
/// then `ddc disk agents --json`, which adds the sizes.
///
/// Read-only, and nothing is saved: liveness is read again on each refresh.
/// The runs go to the runner's `diskAgents` lane, beside the queue and the
/// whole-disk scan (ADR §1). Only the Whole disk view refreshes it, when it
/// appears and on Refresh; never the scheduled path.
@MainActor
@Observable
public final class AgentsModel {
    public static let fastArguments = ["disk", "agents", "--json", "--no-sizes"]
    public static let sizesArguments = ["disk", "agents", "--json"]

    public private(set) var report: AgentsReport?
    /// Set from the start of a refresh until its sizes run ends.
    public private(set) var refreshing = false
    /// The sizes run is going; the sessions are already shown.
    public private(set) var measuring = false
    /// Why the last refresh gave no report, or no sizes.
    public private(set) var message: String?
    public private(set) var now: Date

    @ObservationIgnored private let runner: (any CLIRunning)?
    @ObservationIgnored private let home: String
    @ObservationIgnored private let clock: @MainActor () -> Date
    @ObservationIgnored private let log: @MainActor (String) -> Void
    @ObservationIgnored private var refreshTask: Task<Void, Never>?

    /// `runner` nil: the embedded CLI is missing.
    public init(
        runner: (any CLIRunning)?,
        home: String = NSHomeDirectory(),
        clock: @escaping @MainActor () -> Date = { .now },
        log: @escaping @MainActor (String) -> Void = { _ in }
    ) {
        self.runner = runner
        self.home = home
        self.clock = clock
        self.log = log
        now = clock()
    }

    public var canRefresh: Bool { runner != nil }

    public var summary: AgentsSummary? {
        report.map { AgentsSummary.make($0, home: home, now: now) }
    }

    /// Both runs, one after the other. A refresh asked for while one is
    /// going joins it: it starts no third run.
    public func refresh() async {
        if let refreshTask {
            await refreshTask.value
            return
        }
        guard let runner else {
            message = "The app's embedded CLI is missing, so it cannot list the coding agents. Reinstall DeepDriveCleaner."
            return
        }
        let task = Task { await run(runner) }
        refreshTask = task
        await task.value
        refreshTask = nil
    }

    private func run(_ runner: any CLIRunning) async {
        message = nil
        refreshing = true
        defer {
            refreshing = false
            measuring = false
        }
        let fast = await runner.run(.diskAgents, Self.fastArguments, options: CLIRunner.Options())
        now = clock()
        switch await Self.decode(fast) {
        case .success(let read):
            report = read
        case .failure(let failure):
            // An old list would show sessions that may have ended.
            report = nil
            message = failure.message
            log("disk agents --no-sizes failed: \(failure)")
            return
        }
        measuring = true
        let full = await runner.run(.diskAgents, Self.sizesArguments, options: CLIRunner.Options())
        now = clock()
        switch await Self.decode(full) {
        case .success(let read):
            report = read
        case .failure(let failure):
            // The sessions of the first report stay, without sizes.
            message = "The sizes could not be measured: \(failure.message)"
            log("disk agents failed: \(failure)")
        }
    }

    private static func decode(_ result: CLIRunner.Result) async -> Result<AgentsReport, Failure> {
        guard result.outcome == .done else {
            return .failure(Failure(message: failure(result.outcome, stderrTail: result.stderrTail)))
        }
        let stdout = result.stdout
        let decoded = await Task.detached { Result { try AgentsReport.read(stdout) } }.value
        switch decoded {
        case .success(let read):
            return .success(read)
        case .failure(ScanReport.ReadError.otherSchema(let version)):
            return .failure(Failure(message: "The CLI wrote another version of the report (schema \(version))."))
        case .failure:
            return .failure(Failure(message: "The CLI's report could not be read."))
        }
    }

    struct Failure: Error, CustomStringConvertible {
        let message: String
        var description: String { message }
    }

    static func failure(_ outcome: RunOutcome, stderrTail: [String]) -> String {
        switch outcome {
        case .done:
            return "The CLI's report could not be read."
        case .cancelled:
            return "The run was stopped."
        case .error(let status):
            let error = stderrTail.last { $0.hasPrefix("ddc: ") && !$0.hasPrefix("ddc: warning:") }
            return error.map { String($0.dropFirst("ddc: ".count)) } ?? "Listing the coding agents failed (exit \(status))."
        case .busy:
            return "Listing the coding agents reported busy (exit 75), which it should never do."
        case .refused:
            return "The CLI refused to list the coding agents (exit 2): a bug in the app."
        case .crashed(let signal):
            return "Listing the coding agents stopped unexpectedly (signal \(signal))."
        case .notLaunched(let reason):
            return "Listing the coding agents could not start: \(reason)."
        }
    }
}
