/// What the result screen shows of a clean report (artboard 6): what was
/// done, the free space before and after, and what was left alone grouped by
/// cause, in plain language.
public struct CleanResult: Sendable, Equatable {
    public struct Tally: Sendable, Equatable {
        public var count = 0
        public var bytes: UInt64 = 0
    }

    public struct Volume: Sendable, Equatable {
        /// "The startup disk", or the volume's name.
        public let name: String
        public let freeBefore: UInt64
        public let freeAfter: UInt64
        /// From the scan report; nil when it did not list the volume.
        public let total: UInt64?
    }

    /// One cause of skips: "Spotify, Chrome and Brave were running".
    public struct Cause: Sendable, Equatable {
        public let title: String
        public let count: Int
        public let bytes: UInt64
        /// The items, under `~`.
        public let paths: [String]
    }

    public struct Failure: Sendable, Equatable {
        public let path: String
        public let error: String
    }

    public let deleted: Tally
    public let trashed: Tally
    /// Items cleaned by their tool's commands.
    public let toolsRun: Tally
    public let skipped: Tally
    /// The report's count: failed items and tool commands that failed.
    public let failed: Int
    /// Left by a cancel before they started.
    public let notRun: Tally
    public let cancelled: Bool
    public let volumes: [Volume]
    /// Biggest first.
    public let causes: [Cause]
    public let failures: [Failure]
    public let localSnapshots: Int
    public let journal: String
    /// The items that stay selected for the next clean (`Outcome.staysSelected`).
    public let staysSelected: Int
    /// The skips only a new scan clears (`AfterClean.rescan`): the result offers Scan Again.
    public let needsRescan: Int
    /// The skips that can clear (`AfterClean.keep`), in the report's order:
    /// the result says what clears them.
    public let canClear: [SkipReason]

    /// `confirmation` gives the size of an item the CLI refused before
    /// measuring it; `volumes` give each volume's total.
    public static func make(report: CleanReport, confirmation: Confirmation, volumes: [ScanReport.Volume]) -> CleanResult {
        let home = confirmation.home
        func size(_ item: CleanReport.Item) -> UInt64 {
            if let bytes = item.bytes { return bytes }
            return confirmation.items.first { $0.finding.path == item.path && $0.finding.dev == item.dev && $0.finding.ino == item.ino }?
                .finding.bytes ?? 0
        }
        var deleted = Tally()
        var trashed = Tally()
        var tools = Tally()
        var skipped = Tally()
        var notRun = Tally()
        var failures: [Failure] = []
        var byCause: [CleanWording.Cause: (count: Int, bytes: UInt64, paths: [String])] = [:]
        var causeOrder: [CleanWording.Cause] = []
        var apps: [String] = []
        var canClear: [SkipReason] = []
        var needsRescan = 0
        for item in report.items {
            let bytes = size(item)
            switch item.outcome {
            case .deleted:
                deleted.count += 1
                deleted.bytes += bytes
            case .trashed:
                trashed.count += 1
                trashed.bytes += bytes
            case .native(let commands):
                tools.count += 1
                tools.bytes += bytes
                for command in commands where !command.success {
                    failures.append(Failure(path: Wording.tilde(item.path, home: home), error: failure(command)))
                }
            case .skipped(let reason):
                skipped.count += 1
                skipped.bytes += bytes
                let cause = CleanWording.Cause.of(reason)
                if case .ownerRunning(_, let name) = reason, !apps.contains(name) { apps.append(name) }
                switch reason.afterClean {
                case .keep: canClear.append(reason)
                case .rescan: needsRescan += 1
                case .never: break
                }
                if byCause[cause] == nil { causeOrder.append(cause) }
                byCause[cause, default: (0, 0, [])].count += 1
                byCause[cause, default: (0, 0, [])].bytes += bytes
                byCause[cause, default: (0, 0, [])].paths.append(Wording.tilde(item.path, home: home))
            case .failed(let error):
                failures.append(Failure(path: Wording.tilde(item.path, home: home), error: error))
            case .notRun:
                notRun.count += 1
                notRun.bytes += bytes
            }
        }
        let causes = causeOrder.compactMap { cause in
            byCause[cause].map { Cause(title: CleanWording.title(cause, apps: apps), count: $0.count, bytes: $0.bytes, paths: $0.paths) }
        }
        // Biggest first; the order they were met breaks ties.
        let sorted = causes.enumerated().sorted { a, b in
            a.element.bytes != b.element.bytes ? a.element.bytes > b.element.bytes : a.offset < b.offset
        }.map(\.element)
        return CleanResult(
            deleted: deleted,
            trashed: trashed,
            toolsRun: tools,
            skipped: skipped,
            failed: Int(report.failed),
            notRun: notRun,
            cancelled: report.cancelled,
            volumes: report.volumes.map { volume in
                Volume(
                    name: Wording.volume(volume.mountPoint),
                    freeBefore: volume.freeBefore,
                    freeAfter: volume.freeAfter,
                    total: volumes.first { $0.mountPoint == volume.mountPoint }?.totalBytes
                )
            },
            causes: sorted,
            failures: failures,
            localSnapshots: report.localSnapshots,
            journal: report.journal,
            staysSelected: report.items.filter(\.outcome.staysSelected).count,
            needsRescan: needsRescan,
            canClear: canClear
        )
    }

    /// "docker image prune failed: <its last line>", or "(exit 1)" when it printed nothing.
    static func failure(_ command: CleanReport.CommandResult) -> String {
        "\(name(command)) failed\(said(command))"
    }

    /// What a failed command said, after "failed": ": <its last line>", or
    /// " (exit 1)" when it printed nothing.
    static func said(_ command: CleanReport.CommandResult) -> String {
        let said = command.output.split(whereSeparator: \.isNewline).map { $0.trimmingCharacters(in: .whitespaces) }.last { !$0.isEmpty }
        if let said { return ": \(said)" }
        return command.code.map { " (exit \($0))" } ?? " (interrupted)"
    }

    /// A command without its options: `--name value` pairs and one-letter
    /// flags go, so `docker --context desktop-linux image prune -f` reads
    /// "docker image prune" and `go clean -modcache` stays whole.
    static func name(_ command: CleanReport.CommandResult) -> String {
        var words = [command.program]
        var args = command.args[...]
        while let arg = args.popFirst() {
            if arg.hasPrefix("--"), !arg.contains("="), let value = args.first, !value.hasPrefix("-") {
                args.removeFirst()
            } else if !(arg.hasPrefix("-") && arg.count == 2) {
                words.append(arg)
            }
        }
        return words.joined(separator: " ")
    }
}
