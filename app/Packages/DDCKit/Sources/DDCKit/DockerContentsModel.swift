import Foundation
import Observation

/// What the Review window's Docker Desktop item says is inside its disk
/// image (story "Docker Desktop: what is inside its disk image, and what a
/// clean frees"; menu bar ADR §6 item 10): `ddc disk docker --json` in the
/// runner's `dockerContents` lane, run when the item is shown, never by a
/// scan, the background scan, a dry run or the clean.
///
/// One query at a time. A result stays until a clean (`forget()`), so the
/// sheet can count its estimate; Stop or a closed window ends a query.
@MainActor
@Observable
public final class DockerContentsModel {
    public static let arguments = ["disk", "docker", "--json"]
    /// The rule of the item it describes.
    public static let rule = "vm.docker-desktop"

    public enum State: Sendable, Equatable {
        /// Not asked since the window opened or the last clean.
        case unknown
        case reading
        case listed(DockerContents.Listed)
        /// Docker did not say: why, in words, and what to do when there is
        /// something to do.
        case unavailable(reason: String, advice: String?)
    }

    public private(set) var state: State = .unknown
    /// Grows each time what docker said is let go (a clean, a closed
    /// window): the row's `.task(id:)` asks again while it is shown.
    public private(set) var generation = 0

    /// Told when a query ends or an answer is let go: the sheet counts
    /// the estimate, or stops counting it.
    @ObservationIgnored var onChange: (@MainActor () -> Void)?
    @ObservationIgnored private let runner: (any CLIRunning)?
    @ObservationIgnored private let log: @MainActor (String) -> Void
    @ObservationIgnored private var ticket: CLIRunner.Ticket?
    /// The query running now; a new one waits for it to end, so it never
    /// joins a run that is being stopped.
    @ObservationIgnored private var running: Task<Void, Never>?
    /// Stop was pressed: the row then says so, with Try Again.
    @ObservationIgnored private var stoppedByUser = false

    init(runner: (any CLIRunning)?, log: @escaping @MainActor (String) -> Void) {
        self.runner = runner
        self.log = log
    }

    /// What a clean of the item would free, about; nil until a query listed
    /// it, or when docker could not size a dangling image.
    public var estimate: UInt64? {
        guard case .listed(let listed) = state else { return nil }
        return listed.estimate.bytes
    }

    /// The item is shown: asks docker unless a result is in or a query runs.
    public func show() async {
        guard state == .unknown else { return }
        await read()
    }

    /// Try Again: a new query, whatever the last one said.
    public func again() async {
        guard state != .reading else { return }
        await read()
    }

    /// The row's Stop: the query ends, and the row says it was stopped.
    public func stop() {
        guard state == .reading else { return }
        stoppedByUser = true
        cancel()
    }

    /// A clean ran, or the window closed: what docker said is out of date,
    /// and a query still running ends. The next display asks again.
    public func forget() {
        cancel()
        ticket = nil
        state = .unknown
        generation += 1
        onChange?()
    }

    private func cancel() {
        guard let ticket, let runner else { return }
        Task { await runner.cancel(ticket) }
    }

    private func read() async {
        guard let runner else {
            state = .unavailable(reason: "The app's embedded CLI is missing, so it cannot ask Docker. Reinstall DeepDriveCleaner.", advice: nil)
            return
        }
        let previous = running
        let ticket = CLIRunner.Ticket()
        self.ticket = ticket
        stoppedByUser = false
        state = .reading
        let task = Task { [runner] in
            _ = await previous?.value
            let result = await runner.run(.dockerContents, Self.arguments, options: CLIRunner.Options(ticket: ticket))
            // A clean's `forget()` or a newer query took over meanwhile.
            guard self.ticket == ticket, self.state == .reading else { return }
            self.ticket = nil
            self.state = result.outcome == .cancelled && self.stoppedByUser
                ? .unavailable(reason: "Stopped.", advice: nil)
                : Self.state(result, log: self.log)
            self.onChange?()
        }
        running = task
        await task.value
        if running == task { running = nil }
    }

    static func state(_ result: CLIRunner.Result, log: @MainActor (String) -> Void) -> State {
        guard result.outcome == .done else {
            log("disk docker ended \(result.outcome): \(result.stderrTail.suffix(3).joined(separator: " | "))")
            if result.outcome == .cancelled { return .unknown }
            return .unavailable(reason: failure(result.outcome, stderrTail: result.stderrTail), advice: nil)
        }
        let contents: DockerContents
        do {
            contents = try DockerContents.read(result.stdout)
        } catch ScanReport.ReadError.otherSchema(let version, _) {
            return .unavailable(reason: "The CLI wrote another version of Docker's answer (schema \(version)).", advice: nil)
        } catch {
            log("disk docker unreadable: \(error)")
            return .unavailable(reason: "The CLI's answer about Docker could not be read.", advice: nil)
        }
        return state(contents.result)
    }

    /// Why a run gave no answer. Every answer docker gives exits 0, so any
    /// other ending is the CLI's, not docker's.
    static func failure(_ outcome: RunOutcome, stderrTail: [String]) -> String {
        switch outcome {
        case .done, .cancelled:
            return "The CLI's answer about Docker could not be read."
        case .error(let status):
            let error = stderrTail.last { $0.hasPrefix("ddc: ") && !$0.hasPrefix("ddc: warning:") }
            return error.map { String($0.dropFirst("ddc: ".count)) } ?? "Asking Docker failed (exit \(status))."
        case .busy:
            return "Asking Docker reported busy (exit 75), which it should never do."
        case .refused:
            return "The CLI refused to ask Docker (exit 2): a bug in the app."
        case .crashed(let signal):
            return "Asking Docker stopped unexpectedly (signal \(signal))."
        case .notLaunched(let reason):
            return "Asking Docker could not start: \(reason)."
        }
    }

    /// The CLI's result in the row's words (the advice of story "VM images:
    /// docker prune runs only while Docker Desktop answers").
    static func state(_ result: DockerContents.Result) -> State {
        switch result {
        case .listed(let listed):
            return .listed(listed)
        case .notInstalled:
            return .unavailable(reason: "Docker's command-line tool was not found.", advice: nil)
        case .notReady(let detail) where detail == CleanWording.dockerDown:
            return .unavailable(reason: "\(detail).", advice: "Start Docker Desktop to see what is inside.")
        case .notReady(let detail) where detail == CleanWording.dockerNoContext:
            return .unavailable(
                reason: "Docker did not answer: \(detail).", advice: "If you no longer use Docker Desktop, its folder is yours to delete in Finder."
            )
        case .notReady(let detail):
            return .unavailable(reason: "Docker did not answer: \(Self.sentence(detail))", advice: nil)
        case .failed(let detail):
            return .unavailable(reason: "docker system df failed: \(Self.sentence(detail))", advice: nil)
        case .timedOut(let seconds):
            return .unavailable(reason: "docker system df did not finish within \(seconds) seconds.", advice: nil)
        case .unreadable(let detail):
            return .unavailable(reason: "Docker printed what ddc cannot read: \(Self.sentence(detail))", advice: nil)
        }
    }

    /// `text` ending with one full stop.
    private static func sentence(_ text: String) -> String {
        var trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        while trimmed.hasSuffix(".") { trimmed.removeLast() }
        return trimmed + "."
    }
}
