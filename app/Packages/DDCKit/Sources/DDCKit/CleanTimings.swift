import Foundation

/// Where a clean's time went before its first action: one line in the app
/// log per clean (story "Faster clean: re-check only what the selection
/// needs", decomposition step 1 and AC 9). The instants are taken on the
/// runner's reader thread as each line arrives, never on the main actor.
///
/// Exact, up to the pipe: `preparing` (after the lock), the re-check's last
/// tick (written as its walk ends, unless it repeats the tick before it,
/// which the CLI drops) and `cleaning` (immediately before the first action). The keep-marker walk is only bracketed by its ticks, at
/// most 100 ms apart: the hook's final tick comes after the process snapshot.
public struct CleanTimings: Sendable, Equatable {
    public typealias Instant = ContinuousClock.Instant

    let requested: Instant
    private var started: Instant?
    private var preparing: Instant?
    private var recheckEnd: Instant?
    private var recheckFiles: UInt64?
    /// The first marker tick with a file read: the snapshot count is done.
    private var markersFirst: Instant?
    /// Marker ticks before the hook's final one, which comes with `cleaning`.
    private var markersLast: Instant?
    private var markersPending: Instant?
    private var cleaning: Instant?
    private var ended: Instant?

    /// A marker tick this close before `cleaning` is the hook's final one.
    static let sameWrite: Duration = .milliseconds(20)

    public init(requested: Instant) {
        self.requested = requested
    }

    public mutating func started(at instant: Instant) {
        started = instant
    }

    public mutating func record(_ event: ProgressEvent, at instant: Instant) {
        switch event {
        case .phase(.preparing):
            preparing = preparing ?? instant
        case .phase(.cleaning):
            if cleaning == nil {
                if let pending = markersPending, instant - pending > Self.sameWrite { markersLast = pending }
                cleaning = instant
            }
        case .walk(let walk) where cleaning == nil:
            switch walk.step {
            case .recheck:
                recheckEnd = instant
                recheckFiles = walk.files
            case .markers:
                if walk.files > 0, markersFirst == nil { markersFirst = instant }
                if let pending = markersPending { markersLast = pending }
                markersPending = instant
            case .scan:
                break
            }
        case .walk, .items, .item:
            break
        }
    }

    public mutating func ended(at instant: Instant) {
        ended = instant
    }

    /// "clean timings: waited 0.0 s · re-check 28.4 s (71,250 files) · then
    /// 0.6 s to the first action (marker walk about 0.3 s) · first action
    /// 29.1 s after Clean · cleaning 0.8 s"; a part without its events reads "?".
    public var line: String {
        func span(_ from: Instant?, _ to: Instant?) -> String {
            guard let from, let to else { return "?" }
            let seconds = Double((to - from).components.seconds) + Double((to - from).components.attoseconds) / 1e18
            return String(format: "%.1f s", max(0, seconds))
        }
        let files = recheckFiles.map { " (\(Wording.number(Int(clamping: $0))) files)" } ?? ""
        let markers = markersFirst.flatMap { first in markersLast.map { span(first, $0) } } ?? "?"
        return [
            "clean timings: waited \(span(requested, started))",
            "re-check \(span(preparing, recheckEnd))\(files)",
            "then \(span(recheckEnd, cleaning)) to the first action (marker walk about \(markers))",
            "first action \(span(requested, cleaning)) after Clean",
            "cleaning \(span(cleaning, ended))",
        ].joined(separator: " · ")
    }
}
