/// What the status item shows (artboard 3 of the canvas, with the owner's
/// changes of 2026-10-19: no threshold dot, a warning for what needs
/// attention, grey while paused), as a pure function of the app's state, so
/// every combination is a unit test.
public struct IconState: Sendable, Equatable {
    /// Inside the drive: its dot when idle, a turning arc while the CLI runs,
    /// nothing when a mark sits on the corner.
    public enum Inner: Sendable, Equatable {
        case dot, arc, none
    }

    /// On the drive's top-right corner.
    public enum Mark: Sendable, Equatable {
        case none
        /// A notification was posted and the Review window has not opened since.
        case badge
        /// Something needs the user (`Attention`).
        case warning
    }

    /// Why the icon shows its warning, most important first.
    public enum Attention: Sendable, Equatable, CaseIterable {
        case fullDiskAccess
        case missingCLI
        case scanFailed
        case unreadableReport
        case unreadableSettings

        var words: String {
            switch self {
            case .fullDiskAccess: "needs Full Disk Access"
            case .missingCLI: "its command-line tool is missing"
            case .scanFailed: "the last scan failed"
            case .unreadableReport: "the last report cannot be read"
            case .unreadableSettings: "settings.json cannot be read"
            }
        }
    }

    public let inner: Inner
    public let mark: Mark
    /// Paused: the glyph is drawn grey, as Google Drive greys its icon.
    public let dimmed: Bool
    /// The size next to the icon.
    public let label: String?
    public let accessibility: String

    /// - `busy`: a scan is in flight.
    /// - `cleaning`: a clean, or the rescan after it, is in flight (story "Review and clean window").
    /// - `paused`: Pause in the menu; scans wait for Resume.
    /// - `attention`: what needs the user, if anything; the most important when several do.
    /// - `reclaimable`: what the default selection frees, nil without a report.
    public static func make(
        busy: Bool,
        cleaning: Bool = false,
        paused: Bool = false,
        attention: [Attention] = [],
        reclaimable: UInt64?,
        threshold: UInt64,
        showSize: Bool,
        badgePending: Bool,
        showBadge: Bool
    ) -> IconState {
        let over = reclaimable.map { $0 > 0 && $0 >= threshold } ?? false
        let first = Attention.allCases.first(where: attention.contains)
        let mark: Mark =
            if first != nil { .warning }
            else if badgePending && showBadge { .badge }
            else { .none }
        let inner: Inner = busy || cleaning ? .arc : (mark == .none ? .dot : .none)
        let label = over && showSize ? reclaimable.map(ByteSize.human) : nil

        var words = ["DeepDriveCleaner"]
        if paused { words.append("paused") }
        if cleaning { words.append("cleaning") } else if busy { words.append("scanning") }
        if let first { words.append(first.words) }
        if mark == .badge { words.append("new results to review") }
        if over, let reclaimable { words.append("\(ByteSize.human(reclaimable)) can be freed") }
        return IconState(inner: inner, mark: mark, dimmed: paused, label: label, accessibility: words.joined(separator: ", "))
    }
}
