import Foundation

/// A clean's progress as the sheet shows it (artboard 11 of the canvas),
/// folded from the CLI's events (story "CLI: progress events"). The phase
/// only moves forward, a `walk` tick after `cleaning` is ignored, counters
/// take the larger value, and an `item` line already read changes nothing.
/// Item lines come in order, so the highest `i` read stands for all of them.
public struct CleanProgress: Sendable, Equatable {
    /// Step 1's walk: the re-check, then the keep-marker walk.
    public struct Walk: Sendable, Equatable {
        public let step: ProgressEvent.WalkStep
        public fileprivate(set) var files: UInt64
        /// An estimate; nil or 0 when there is none.
        public fileprivate(set) var expected: UInt64?
    }

    public private(set) var walk: Walk?
    /// The `cleaning` event came: from then on the clean may have touched files.
    public private(set) var cleaning = false
    /// Items with an outcome other than `not-run`.
    public private(set) var done: UInt64 = 0
    /// Items the CLI runs.
    public private(set) var n: UInt64 = 0
    /// Both without `native-only` items, whose space is not counted.
    public private(set) var bytesDone: UInt64 = 0
    public private(set) var bytesTotal: UInt64 = 0
    public private(set) var current: ProgressEvent.Current?
    public private(set) var skipped = 0
    /// The highest `i` of the `item` lines read: the CLI writes them in order.
    private var lastItem: UInt64 = 0
    /// What the `item` lines alone count: the ticks lag them.
    private var itemsDone: UInt64 = 0
    private var itemBytes: UInt64 = 0

    public init() {}

    public mutating func apply(_ event: ProgressEvent) {
        switch event {
        case .phase(.preparing):
            break
        case .phase(.cleaning):
            cleaning = true
        case .walk(let tick):
            guard !cleaning else { return }
            if let walk, walk.step == tick.step {
                self.walk?.files = max(walk.files, tick.files)
                self.walk?.expected = tick.filesExpected ?? walk.expected
            } else if walk.map({ Self.order(tick.step) > Self.order($0.step) }) ?? true {
                walk = Walk(step: tick.step, files: tick.files, expected: tick.filesExpected)
            }
        case .items(let tick):
            cleaning = true
            n = max(n, tick.n)
            done = max(done, tick.done)
            bytesDone = max(bytesDone, tick.bytesDone)
            bytesTotal = max(bytesTotal, tick.bytesTotal)
            current = tick.current.flatMap { $0.i <= lastItem ? nil : $0 }
        case .item(let line):
            cleaning = true
            guard line.i > lastItem else { return }
            lastItem = line.i
            n = max(n, line.n)
            if current?.i == line.i { current = nil }
            if case .skipped = line.outcome { skipped += 1 }
            guard line.outcome != .notRun else { return }
            itemsDone += 1
            done = max(done, itemsDone)
            if line.action != .nativeOnly {
                itemBytes += line.bytes
                bytesDone = max(bytesDone, itemBytes)
            }
        }
    }

    /// Until the CLI's first count, the sheet's own: the bar never reads
    /// "0 of 0". The CLI's `n` leaves out items refused at its re-check.
    public func counting(items: Int, bytes: UInt64) -> CleanProgress {
        guard n == 0 else { return self }
        var progress = self
        progress.n = UInt64(items)
        progress.bytesTotal = bytes
        return progress
    }

    private static func order(_ step: ProgressEvent.WalkStep) -> Int {
        ProgressEvent.WalkStep.allCases.firstIndex(of: step) ?? 0
    }

    /// Step 1's bar: files read of the estimate, below 1 while the walk
    /// runs; nil without an estimate.
    public var walkFraction: Double? {
        guard let walk, let expected = walk.expected, expected > 0 else { return nil }
        return min(Double(walk.files) / Double(expected), 0.99)
    }

    /// Step 2's bar: bytes processed of the total, or items when no byte
    /// is counted; below 1 until every item is done.
    public var fraction: Double {
        let value = if bytesTotal > 0 {
            Double(bytesDone) / Double(bytesTotal)
        } else if n > 0 {
            Double(done) / Double(n)
        } else {
            0.0
        }
        return min(value, done < n ? 0.99 : 1)
    }

    /// The striped segment after the bar: the item in flight. None for a
    /// `native-only` item, whose space is not in the total.
    public var inFlight: Double {
        guard let current, current.action != .nativeOnly else { return 0 }
        let share = if bytesTotal > 0 {
            Double(current.bytes) / Double(bytesTotal)
        } else if n > 0 {
            1 / Double(n)
        } else {
            0.0
        }
        return max(0, min(share, 1 - fraction))
    }
}

extension CleanWording {
    /// Step 1's line: "45,600 of about 71,300 files read".
    public static func walkLine(_ walk: CleanProgress.Walk) -> String {
        let read = Wording.number(Int(clamping: walk.files))
        guard let expected = walk.expected, expected > 0 else { return "\(read) files read" }
        return "\(read) of about \(Wording.number(Int(clamping: max(expected, walk.files)))) files read"
    }

    /// Step 1's explanation.
    public static func walkDetail(_ step: ProgressEvent.WalkStep?) -> String {
        switch step {
        case .markers:
            "The confirmed items are read for .ddc-keep files, so nothing you protected is cleaned."
        case .recheck, .scan, .disk, nil:
            "The folders are read once more, so nothing is cleaned from an outdated scan. The total comes from your last scan."
        }
    }

    /// Step 2's line: "6.2 of 13.9 GiB processed · 34 of 90 items · 2 skipped";
    /// items first when no byte is counted.
    public static func counterLine(_ progress: CleanProgress) -> String {
        let bytes = "\(Wording.progress(progress.bytesDone, of: progress.bytesTotal)) processed"
        let items = "\(Wording.number(Int(clamping: progress.done))) of \(Wording.items(Int(clamping: progress.n)))"
        var parts = progress.bytesTotal > 0 ? [bytes, items] : ["\(items) processed"]
        if progress.skipped > 0 { parts.append("\(Wording.number(progress.skipped)) skipped") }
        return parts.joined(separator: " · ")
    }

    /// "44%": the bar's fraction, rounded down.
    public static func percent(_ fraction: Double) -> String {
        "\(Int((max(0, min(fraction, 1)) * 100).rounded(.down)))%"
    }

    /// What runs now: the tool while its command runs, else the item's path.
    public static func now(_ current: ProgressEvent.Current, home: String) -> String {
        current.tool.map(toolName) ?? Wording.tilde(current.path, home: home)
    }

    /// Step 2's explanation, for the item in flight.
    public static func cleaningNote(_ progress: CleanProgress) -> String {
        var parts: [String] = []
        let tool = progress.current?.tool.map(toolName)
        if let tool {
            let image = progress.current?.action == .nativeOnly ? ", and what it frees inside its disk image is not counted here" : ""
            parts.append("\(tool) reports no progress\(image).")
        } else if progress.inFlight > 0 {
            parts.append("The striped part is the item being cleaned.")
        }
        if progress.skipped > 0 {
            parts.append("\(Wording.items(progress.skipped)) \(progress.skipped == 1 ? "was" : "were") skipped; the result says why.")
        }
        parts.append(tool.map { "Stop interrupts \($0); what it already removed stays removed." }
            ?? "Stop ends the clean after this item; what is cleaned stays cleaned.")
        return parts.joined(separator: " ")
    }

    /// A program as the sheet names it: "Docker", "uv".
    public static func toolName(_ program: String) -> String {
        switch program {
        case "docker": "Docker"
        case "podman": "Podman"
        default: program
        }
    }

    /// The runtime of a `native-only` rule, whose space is freed inside its disk image.
    public static func runtime(rule: String) -> String {
        switch rule {
        case "vm.docker-desktop": "Docker"
        case "vm.podman": "Podman"
        case "vm.colima": "Colima"
        case "vm.lima": "Lima"
        case "vm.orbstack": "OrbStack"
        default: String(rule.split(separator: ".").last ?? Substring(rule))
        }
    }

    /// Under the sheet's header when `native-only` items are listed: "4.2 GiB
    /// inside Docker's disk image is pruned there and not counted". Nil without one.
    public static func notCounted(_ confirmation: Confirmation) -> String? {
        let parts = confirmation.notCounted.map { "\(ByteSize.human($0.bytes)) inside \($0.runtime)'s disk image" }
        guard !parts.isEmpty else { return nil }
        return "\(Wording.list(parts)) \(parts.count == 1 ? "is" : "are") pruned there and not counted."
    }
}
