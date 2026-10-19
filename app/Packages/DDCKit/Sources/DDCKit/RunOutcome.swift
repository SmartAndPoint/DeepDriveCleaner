import Foundation

/// How a run of the CLI ended, from its exit status (ADR §1).
public enum RunOutcome: Sendable, Equatable {
    /// Exit 0.
    case done
    /// Exit 1, or any status the CLI does not document.
    case error(status: Int32)
    /// Exit 2: the CLI refused the request (a stale plan, a bad argument).
    case refused
    /// Exit 75 (`EX_TEMPFAIL`): another scan or clean holds the lock.
    case busy
    /// Ended by the SIGTERM of `CLIRunner.cancel(_:)`.
    case cancelled
    /// Ended by a signal nobody sent on purpose (a crash, an abort, the kernel).
    case crashed(signal: Int32)
    /// The executable could not be started.
    case notLaunched(reason: String)

    /// `status` is the signal number when `reason` is `.uncaughtSignal`.
    public init(status: Int32, reason: Process.TerminationReason, cancelRequested: Bool) {
        if reason == .uncaughtSignal {
            self = cancelRequested ? .cancelled : .crashed(signal: status)
            return
        }
        switch status {
        case 0: self = .done
        case 2: self = .refused
        case 75: self = .busy
        default: self = .error(status: status)
        }
    }
}
