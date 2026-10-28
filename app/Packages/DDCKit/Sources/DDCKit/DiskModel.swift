import Foundation
import Observation

/// The Whole disk view's state (story "Whole disk in the app", artboard 13):
/// the last `last-disk.json`, Scan whole disk with its progress, and Stop.
///
/// Read-only: nothing here frees space. `ddc disk` takes no lock, so its run
/// goes to the runner's disk lane, beside the queue (ADR §1, amended
/// 2026-10-28). The app never starts it on its own, and leaves a running one
/// alone on quit: the CLI saves its report before it prints.
@MainActor
@Observable
public final class DiskModel {
    public enum ReportState: Equatable {
        case loading
        case none
        case unreadable(String)
        case loaded(DiskReport)
    }

    /// The running whole-disk scan.
    public struct Progress: Equatable, Sendable {
        public let started: Date
        /// From the CLI's `walk` events with step `disk`; nil before the first.
        public var files: UInt64?
        public var bytes: UInt64?
        /// The CLI's estimate of the files it will read; `disk` gives none
        /// today, so the bar is indeterminate with counters.
        public var filesExpected: UInt64?

        /// 0...1 when the CLI gave an estimate.
        public var fraction: Double? {
            guard let files, let filesExpected, filesExpected > 0 else { return nil }
            return min(1, Double(files) / Double(filesExpected))
        }
    }

    public private(set) var report: ReportState = .loading
    public private(set) var progress: Progress?
    /// Why the last run did not give a report; the report before it stays.
    public private(set) var message: String?
    public private(set) var now: Date

    @ObservationIgnored private let runner: (any CLIRunning)?
    @ObservationIgnored private let data: DataDirectory
    @ObservationIgnored private let clock: @MainActor () -> Date
    @ObservationIgnored private let log: @MainActor (String) -> Void
    @ObservationIgnored private var reportModified: Date?
    @ObservationIgnored private var scanTask: Task<Void, Never>?
    @ObservationIgnored private var ticket: CLIRunner.Ticket?

    /// `runner` nil: the embedded CLI is missing.
    public init(
        runner: (any CLIRunning)?,
        data: DataDirectory,
        clock: @escaping @MainActor () -> Date = { .now },
        log: @escaping @MainActor (String) -> Void = { _ in }
    ) {
        self.runner = runner
        self.data = data
        self.clock = clock
        self.log = log
        now = clock()
    }

    public var canScan: Bool { runner != nil }
    public var scanning: Bool { progress != nil }

    public var loadedReport: DiskReport? {
        if case .loaded(let report) = report { return report }
        return nil
    }

    /// "Measured today at 8:04 in 13.0 s", or nil without a report.
    public var subtitle: String? {
        guard let report = loadedReport, let date = Wording.date(rfc3339: report.generatedAt) else { return nil }
        return "Measured \(Wording.moment(date, now: now)) in \(Wording.duration(milliseconds: report.durationMs))"
    }

    /// The bar and the rows; `scan`, `summary` and `included` are the
    /// popover's, so the home folder's coloured part is the popover's number.
    public func summary(scan: ScanReport?, summary: ReportSummary?, included: Set<ScanReport.RiskClass>) -> DiskSummary? {
        loadedReport.map { DiskSummary.make($0, scan: scan, summary: summary, included: included, now: now) }
    }

    // MARK: - The report on disk

    /// When the view appears, and after each run: decodes `last-disk.json`
    /// again only when its modification date changed.
    public func load() async {
        now = clock()
        let url = data.lastDisk
        let modified = DataDirectory.modified(url)
        guard report == .loading || modified != reportModified else { return }
        guard let modified else {
            reportModified = nil
            report = .none
            return
        }
        let decoded = await Task.detached { Result { try DiskReport.read(Data(contentsOf: url)) } }.value
        reportModified = modified
        apply(decoded)
    }

    private func apply(_ decoded: Result<DiskReport, any Error>) {
        switch decoded {
        case .success(let read):
            report = .loaded(read)
        case .failure(ScanReport.ReadError.otherSchema(let version)):
            report = .unreadable("It was written by another version of ddc (schema \(version)).")
        case .failure(let error):
            report = .unreadable("It is not a whole-disk report this app can read.")
            log("last-disk.json unreadable: \(error)")
        }
    }

    // MARK: - Scan whole disk

    /// One run at a time: a second request while one runs waits for it.
    public func scanDisk() async {
        if let scanTask {
            await scanTask.value
            return
        }
        guard let runner else {
            message = "The app's embedded CLI is missing, so it cannot measure the disk. Reinstall DeepDriveCleaner."
            return
        }
        let task = Task { await run(runner) }
        scanTask = task
        await task.value
        scanTask = nil
    }

    /// Stops the running scan: the CLI prints what it measured so far.
    public func stop() async {
        guard let ticket, let runner else { return }
        log("whole-disk scan: stop")
        await runner.cancel(ticket)
    }

    private func run(_ runner: any CLIRunning) async {
        message = nil
        progress = Progress(started: clock())
        let ticket = CLIRunner.Ticket()
        self.ticket = ticket
        defer {
            self.ticket = nil
            progress = nil
        }
        log("whole-disk scan started")
        let (lines, sink) = AsyncStream<ProgressEvent.Line>.makeStream()
        let consumer = Task { @MainActor in
            for await line in lines {
                guard case .event(.walk(let walk)) = line, walk.step == .disk, var next = progress else { continue }
                next.files = max(next.files ?? 0, walk.files)
                next.bytes = max(next.bytes ?? 0, walk.bytes)
                next.filesExpected = walk.filesExpected ?? next.filesExpected
                progress = next
            }
        }
        let options = CLIRunner.Options(ticket: ticket, stderrLine: { line in
            if let read = ProgressEvent.read(line) { sink.yield(read) }
        })
        let result = await runner.run(.disk, ["disk", "--json", "--progress", "json"], options: options)
        sink.finish()
        await consumer.value
        now = clock()

        // A stopped run first: its stdout holds the partial report.
        let stdout = result.stdout
        if result.outcome == .done || result.outcome == .cancelled, !stdout.isEmpty,
           case .success(let read) = await Task.detached(operation: { Result { try DiskReport.read(stdout) } }).value
        {
            report = .loaded(read)
            reportModified = DataDirectory.modified(data.lastDisk)
            log("whole-disk scan \(result.outcome == .done ? "done" : "stopped")")
            return
        }
        if result.outcome == .cancelled {
            message = "Stopped before a report was written; the last one is shown."
            log("whole-disk scan stopped without a report")
        } else {
            message = Self.failure(result.outcome, stderrTail: result.stderrTail)
                ?? "The whole-disk report could not be read."
            log("whole-disk scan ended \(result.outcome): \(result.stderrTail.suffix(3).joined(separator: " | "))")
        }
        await load()
    }

    /// `CLIFailure.message` speaks of the home folder's scan.
    static func failure(_ outcome: RunOutcome, stderrTail: [String]) -> String? {
        switch outcome {
        case .done, .cancelled:
            return nil
        case .error(let status):
            let error = stderrTail.last { $0.hasPrefix("ddc: ") && !$0.hasPrefix("ddc: warning:") }
            return error.map { String($0.dropFirst("ddc: ".count)) } ?? "The whole-disk scan failed (exit \(status))."
        case .busy:
            return "The whole-disk scan reported busy (exit 75), which it should never do."
        case .refused:
            return "The CLI refused the whole-disk scan (exit 2): a bug in the app."
        case .crashed(let signal):
            return "The whole-disk scan stopped unexpectedly (signal \(signal))."
        case .notLaunched(let reason):
            return "The whole-disk scan could not start: \(reason)."
        }
    }
}
