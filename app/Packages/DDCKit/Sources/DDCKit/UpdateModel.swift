import Foundation
import Observation

/// A found update as `AppPreferences` keeps it. The feed it came from is
/// kept too: Debug and Release builds share one defaults domain, and a Debug
/// build's local feed must never name an update to a Release build.
public struct UpdateRecord: Codable, Sendable, Equatable {
    public let version: String
    public let shortVersion: String
    public let releaseURL: URL?
    public let feedURL: URL
    public let informationOnly: Bool

    public init(version: String, shortVersion: String, releaseURL: URL?, feedURL: URL, informationOnly: Bool) {
        self.version = version
        self.shortVersion = shortVersion
        self.releaseURL = releaseURL
        self.feedURL = feedURL
        self.informationOnly = informationOnly
    }

    var item: UpdateFlow.Item {
        UpdateFlow.Item(version: version, shortVersion: shortVersion, releaseURL: releaseURL)
    }
}

/// The update line's model (story "Sparkle updates: check daily, Update and
/// Relaunch, and the cask's auto_updates"; ADR "Updates through Sparkle,
/// alongside the Homebrew cask", §3 and §4). It holds `UpdateFlow`, acts on
/// the effects that are not Sparkle's to act on, and keeps Sparkle's postpone
/// handler: called once when the runner is idle, dropped when the app quits.
/// The app's Sparkle adapter sends it events and applies the replies `send`
/// returns.
@MainActor
@Observable
public final class UpdateModel {
    public private(set) var flow = UpdateFlow()
    /// The update a check of an earlier launch found, until this session finds or forgets one.
    public private(set) var kept: UpdateRecord?

    /// Starts a check the person asked for (`SPUUpdater.checkForUpdates()`).
    @ObservationIgnored public var startCheck: (@MainActor () -> Void)?
    /// Called when `jobsHeld` changes: the popover and the scheduled work hold their jobs.
    @ObservationIgnored public var onJobsHeld: (@MainActor (Bool) -> Void)?

    @ObservationIgnored private var postponed: (@MainActor () -> Void)?
    @ObservationIgnored private let preferences: AppPreferences
    @ObservationIgnored private let feedURL: URL
    @ObservationIgnored private let busy: @MainActor () -> Bool
    @ObservationIgnored private let log: @MainActor (String) -> Void

    /// `bundleVersion` is the running app's `CFBundleVersion`: a kept update
    /// it reaches or passes was installed, and is forgotten.
    public init(
        preferences: AppPreferences,
        feedURL: URL,
        bundleVersion: String,
        busy: @escaping @MainActor () -> Bool,
        log: @escaping @MainActor (String) -> Void
    ) {
        self.preferences = preferences
        self.feedURL = feedURL
        self.busy = busy
        self.log = log
        guard let record = preferences.foundUpdate else { return }
        // Another build's feed: left for that build, never shown here.
        guard record.feedURL == feedURL else { return }
        if bundleVersion.compare(record.version, options: .numeric) == .orderedAscending {
            kept = record
        } else {
            preferences.setFoundUpdate(nil)
            log("update \(record.shortVersion) (\(record.version)) is installed: forgotten")
        }
    }

    public var state: UpdateFlow.State { flow.state }

    /// The update the line names: this session's, else the kept one.
    public var item: UpdateFlow.Item? { flow.item ?? kept?.item }

    /// The line's update can only be installed by hand.
    public var informationOnly: Bool { flow.item != nil ? flow.itemIsInformationOnly : kept?.informationOnly ?? false }

    /// Update and Relaunch: an installable update is known, nothing else is
    /// under way, and the runner has no work.
    public var canPress: Bool {
        guard item != nil, !informationOnly, !busy() else { return false }
        switch flow.state {
        case .idle, .available, .failed: return true
        default: return false
        }
    }

    /// The relaunch is held or under way: no scan, clean, free or uninstall starts.
    public var jobsHeld: Bool { flow.state == .waitingForIdle || flow.state == .installing }

    /// Update and Relaunch was pressed.
    public func press() {
        guard canPress else { return }
        send(.pressed)
    }

    /// Feeds one event to the flow and acts on its own effects; returns all
    /// of them, the replies to Sparkle included.
    @discardableResult
    public func send(_ event: UpdateFlow.Event) -> [UpdateFlow.Effect] {
        let held = jobsHeld
        let effects = flow.handle(event)
        for effect in effects {
            apply(effect)
        }
        if jobsHeld != held {
            log(jobsHeld ? "update: jobs held until the install" : "update: jobs released")
            onJobsHeld?(jobsHeld)
        }
        return effects
    }

    /// Sparkle's `shouldPostponeRelaunchForUpdate`: true holds the relaunch,
    /// and `resume` is called once, when the runner is next idle.
    public func shouldPostpone(resume: @escaping @MainActor () -> Void) -> Bool {
        guard send(.postponeAsked(busy: busy())).contains(.postpone) else { return false }
        postponed = resume
        log("update: the relaunch waits for the runner")
        return true
    }

    /// `RunnerActivity.onIdle`.
    public func runnerIdle() {
        send(.runnerIdle)
    }

    /// The first thing `applicationShouldTerminate` does.
    public func appQuits() {
        send(.appQuits)
    }

    private func apply(_ effect: UpdateFlow.Effect) {
        switch effect {
        case .record(let item, let informationOnly):
            let record = UpdateRecord(
                version: item.version, shortVersion: item.shortVersion, releaseURL: item.releaseURL,
                feedURL: feedURL, informationOnly: informationOnly)
            kept = record
            preferences.setFoundUpdate(record)
        case .forget:
            kept = nil
            if preferences.foundUpdate?.feedURL == feedURL { preferences.setFoundUpdate(nil) }
        case .checkForUpdates:
            startCheck?()
        case .resumeInstall:
            let resume = postponed
            postponed = nil
            log("update: the runner is idle, the install goes on")
            resume?()
        case .dropPostponed:
            postponed = nil
            log("update: the app quits, the install finishes without a relaunch")
        case .reply, .postpone:
            // The adapter's to give.
            break
        }
    }
}
