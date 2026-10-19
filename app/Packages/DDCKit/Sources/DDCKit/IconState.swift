/// What the status item shows (artboard 3 of the canvas), as a pure function
/// of the app's state, so every combination is a unit test.
public struct IconState: Sendable, Equatable {
    /// Inside the drive: its dot when idle, a turning arc while the CLI runs,
    /// nothing when a mark sits on the corner.
    public enum Inner: Sendable, Equatable {
        case dot, arc, none
    }

    /// On the drive's top-right corner.
    public enum Mark: Sendable, Equatable {
        case none
        /// Over the notification threshold.
        case dot
        /// A notification was posted and the Review window has not opened since.
        case badge
        /// Full Disk Access is missing ("!").
        case alert
    }

    public let inner: Inner
    public let mark: Mark
    /// The size next to the icon.
    public let label: String?
    public let accessibility: String

    /// - `busy`: a scan (or, later, a clean) is in flight.
    /// - `reclaimable`: what the default selection frees, nil without a report.
    public static func make(
        busy: Bool,
        reclaimable: UInt64?,
        threshold: UInt64,
        showSize: Bool,
        fullDiskAccess: Bool,
        badgePending: Bool,
        showBadge: Bool
    ) -> IconState {
        let over = reclaimable.map { $0 > 0 && $0 >= threshold } ?? false
        let mark: Mark =
            if !fullDiskAccess { .alert }
            else if badgePending && showBadge { .badge }
            else if over { .dot }
            else { .none }
        let inner: Inner = busy ? .arc : (mark == .none ? .dot : .none)
        let label = over && showSize ? reclaimable.map(ByteSize.human) : nil

        var words = ["DeepDriveCleaner"]
        if busy { words.append("scanning") }
        switch mark {
        case .alert: words.append("needs Full Disk Access")
        case .badge: words.append("new results to review")
        case .dot, .none: break
        }
        if over, let reclaimable { words.append("\(ByteSize.human(reclaimable)) can be freed") }
        return IconState(inner: inner, mark: mark, label: label, accessibility: words.joined(separator: ", "))
    }
}
