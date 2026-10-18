/// How the app runs after a launch (ADR §5).
///
/// A launch with `--scheduled` comes from the scheduler agent: no status item,
/// and the app quits when its work is done. LaunchServices keeps one instance per
/// bundle id, so a user's launch while that instance runs only reaches it, as a
/// reopen or activate event, or as a notification response. Any of those promotes
/// the instance to the full app, which keeps running: a scheduled instance never
/// swallows a user launch. A further launch from Finder, Spotlight, the login
/// item or `open` arrives as `reopen`.
public struct LaunchMode: Sendable, Equatable {
    public enum Mode: Sendable, Equatable {
        case starting, normal, scheduled
    }

    public enum Event: Sendable, Equatable {
        case launched(arguments: [String])
        case reopen
        case activate
        case notificationResponse
        /// The scheduled work (a scan if due, a pending notification) ended.
        case workFinished
    }

    public enum Effect: Sendable, Equatable {
        case showStatusItem
        case startScheduledWork
        /// A reopen of the full app: the scheduler's tick, or a user's launch.
        case scanIfDue
        case quit
    }

    public static let scheduledArgument = "--scheduled"

    public private(set) var mode: Mode = .starting

    public init() {}

    public mutating func handle(_ event: Event) -> [Effect] {
        switch (mode, event) {
        case (.starting, .launched(let arguments)):
            if arguments.contains(Self.scheduledArgument) {
                mode = .scheduled
                return [.startScheduledWork]
            }
            mode = .normal
            return [.showStatusItem]
        case (.scheduled, .reopen), (.scheduled, .activate), (.scheduled, .notificationResponse):
            mode = .normal
            return [.showStatusItem]
        case (.scheduled, .workFinished):
            return [.quit]
        case (.normal, .reopen):
            return [.scanIfDue]
        default:
            return []
        }
    }
}
