import Foundation

/// How the app runs after a launch (ADR §5).
///
/// A launch with `--scheduled` comes from the scheduler agent: no status item,
/// and the app quits when its work is done. LaunchServices keeps one instance per
/// bundle id, so a user's launch while that instance runs only reaches it, as a
/// reopen or activate event, or as a notification response. Any of those promotes
/// the instance to the full app, which keeps running: a scheduled instance never
/// swallows a user launch. A further launch from Finder, Spotlight, the login
/// item or `open` arrives as `reopen`; so does a click on the Dock icon while a
/// window keeps the app in the Dock (`DockPresence`).
public struct LaunchMode: Sendable, Equatable {
    public enum Mode: Sendable, Equatable {
        case starting, normal, scheduled
    }

    public enum Event: Sendable, Equatable {
        case launched(arguments: [String])
        /// `windowOpen`: one of the app's windows is open, so the app is in the Dock.
        case reopen(windowOpen: Bool)
        case activate
        case notificationResponse
        /// The scheduled work (a scan if due, a pending notification) ended.
        case workFinished
    }

    public enum Effect: Sendable, Equatable {
        case showStatusItem
        /// Start the updater (`SPUUpdater.start()`), once, after the first
        /// status item: a `--scheduled` launch that only does its work and
        /// quits never checks for updates (story "Sparkle updates: check
        /// daily, Update and Relaunch, and the cask's auto_updates").
        case startUpdater
        case startScheduledWork
        /// A reopen of the full app: the scheduler's tick, or a user's launch.
        case scanIfDue
        /// Bring the open windows to the front (a click on the Dock icon). Added
        /// to the scan: a reopen cannot tell the Dock from the scheduler's `open`.
        case frontWindows
        case quit
    }

    public static let scheduledArgument = "--scheduled"
    /// The bundled agent's argument (B6): `main.swift` hands the work on.
    public static let tickArgument = "--tick"

    /// What `--tick` passes to `/usr/bin/open`: the app by its own bundle
    /// path, hidden and in the background, so a Debug copy and a release
    /// copy never launch each other (B6).
    public static func tickOpenArguments(bundleURL: URL) -> [String] {
        ["-gj", bundleURL.path, "--args", scheduledArgument]
    }

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
            return [.showStatusItem, .startUpdater]
        case (.scheduled, .reopen), (.scheduled, .activate), (.scheduled, .notificationResponse):
            mode = .normal
            return [.showStatusItem, .startUpdater]
        case (.scheduled, .workFinished):
            return [.quit]
        case (.normal, .reopen(let windowOpen)):
            return windowOpen ? [.scanIfDue, .frontWindows] : [.scanIfDue]
        default:
            return []
        }
    }
}
