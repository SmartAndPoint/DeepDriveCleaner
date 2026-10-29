import Foundation
import Observation

/// Whether the app's runner has work, asked synchronously (story "Sparkle
/// updates: check daily, Update and Relaunch, and the cask's auto_updates";
/// ADR "Updates through Sparkle, alongside the Homebrew cask", §3): Sparkle's
/// postpone question needs the answer at once, and `CLIRunner` is an actor.
///
/// It wraps the app's one `CLIRunner` and is what every model is given. A run
/// counts from the moment it is asked for, so one waiting in the queue or
/// behind its lane counts too, until its result returns. Runs that join
/// another (a second scan) count each. The runner's state is the only gate:
/// nothing here probes the data directory's lock.
@MainActor
@Observable
public final class RunnerActivity: CLIRunning {
    /// What the update line says it waits for.
    public enum Reason: Sendable, Equatable {
        /// A scan, a measure, a listing, or the scheduler's bookkeeping.
        case scanning
        /// Any other kind: a clean or a `disk free`, or the dry run that
        /// comes before one.
        case cleaning
    }

    /// Runs asked for and not yet returned, per kind.
    public private(set) var inFlight: [CLIRunner.Kind: Int] = [:]
    /// Called once each time the last run in flight returns.
    @ObservationIgnored public var onIdle: (@MainActor () -> Void)?

    @ObservationIgnored private let inner: any CLIRunning

    public init(_ inner: any CLIRunning) {
        self.inner = inner
    }

    /// A run is asked for and has not returned, in the queue or in any lane.
    public var busy: Bool { !inFlight.isEmpty }

    /// Nil when idle; a clean wins over a scan that runs beside it.
    public var reason: Reason? {
        guard busy else { return nil }
        return inFlight.keys.allSatisfy(Self.scans.contains) ? .scanning : .cleaning
    }

    /// The kinds the line calls "the scan". The clean's kinds are not named
    /// here: only their own files run them (`ContractTests`).
    private static let scans: Set<CLIRunner.Kind> = [.scan, .backgroundScan, .rules, .notify, .schedule, .disk, .diskAgents, .brewPackages]

    public func run(_ kind: CLIRunner.Kind, _ arguments: [String], options: CLIRunner.Options) async -> CLIRunner.Result {
        inFlight[kind, default: 0] += 1
        // Also when the caller's task is cancelled: the run itself always returns.
        defer { finished(kind) }
        return await inner.run(kind, arguments, options: options)
    }

    public func cancel(_ ticket: CLIRunner.Ticket) async {
        await inner.cancel(ticket)
    }

    private func finished(_ kind: CLIRunner.Kind) {
        let left = (inFlight[kind] ?? 1) - 1
        inFlight[kind] = left > 0 ? left : nil
        if inFlight.isEmpty { onIdle?() }
    }
}
