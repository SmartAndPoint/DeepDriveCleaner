import Foundation

/// The update line's state machine (ADR "Updates through Sparkle, alongside
/// the Homebrew cask", §2 to §4). Sparkle's user driver and delegate are an
/// adapter in the app target that feeds it events and acts on its effects;
/// DDCKit never links Sparkle, so this is tested without it.
///
/// Found updates are answered Dismiss, never Skip: with automatic updates
/// off, Dismiss downloads nothing and the next check offers the item again.
/// Only Update and Relaunch installs, and one press is enough: the check it
/// starts finds the item and is answered Install, and so is ready-to-install.
/// An information-only item (a team change) is never installed. While the
/// runner has work, the relaunch is held and resumes when it is idle; a quit
/// meanwhile lets Sparkle finish the install after the app is gone.
public struct UpdateFlow: Sendable, Equatable {
    public enum State: Sendable, Equatable {
        /// No update known this session.
        case idle
        /// An installable update was found and answered Dismiss.
        case available
        /// The update must be installed by hand (`informationalUpdate`).
        case informationOnly
        /// A check the person asked for runs.
        case checking
        case downloading
        case extracting
        /// Install was answered; the relaunch is held while the runner has work.
        case waitingForIdle
        case installing
        /// The app quit while the relaunch was held: Sparkle installs after
        /// it is gone, and nothing relaunches it.
        case installingOnQuit
        /// A check the person asked for failed before any update was known.
        case failed
        /// `SPUUpdater.start()` threw: the rest of the app works as before.
        case unavailable(String)
    }

    /// One appcast item, as the line and the stored record need it.
    public struct Item: Sendable, Equatable, Codable {
        /// `sparkle:version`, the app's `CFBundleVersion`.
        public let version: String
        /// `sparkle:shortVersionString`, the version the line names.
        public let shortVersion: String
        /// `sparkle:fullReleaseNotesLink`, the release page: the manual route.
        public let releaseURL: URL?

        public init(version: String, shortVersion: String, releaseURL: URL?) {
            self.version = version
            self.shortVersion = shortVersion
            self.releaseURL = releaseURL
        }
    }

    public enum Event: Sendable, Equatable {
        /// Update and Relaunch.
        case pressed
        /// A check the person asked for started (Check Now, or the press's own).
        case checkStarted
        case found(Item, informationOnly: Bool)
        case notFound
        case downloadStarted
        case extracting
        case readyToInstall
        /// Sparkle asks whether to hold the relaunch; `busy` is the runner's state now.
        case postponeAsked(busy: Bool)
        /// The runner has no run in flight any more.
        case runnerIdle
        case installStarted
        case error(String)
        case appQuits
        case startFailed(String)
    }

    /// Sparkle's two answers this app gives. There is no Skip.
    public enum Reply: Sendable, Equatable {
        case dismiss, install
    }

    public enum Effect: Sendable, Equatable {
        /// The answer to `found` or `readyToInstall`.
        case reply(Reply)
        /// Start a check the person asked for (`checkForUpdates()`).
        case checkForUpdates
        /// The answer to `postponeAsked`: hold the relaunch; no effect means go on.
        case postpone
        /// Call the stored postpone handler, once.
        case resumeInstall
        /// Forget the stored postpone handler without calling it.
        case dropPostponed
        /// Keep the found item, so the line survives a restart.
        case record(Item, informationOnly: Bool)
        /// The feed has nothing newer: forget the kept item.
        case forget
    }

    public private(set) var state: State = .idle
    /// The last item found this session.
    public private(set) var item: Item?
    public private(set) var itemIsInformationOnly = false
    /// Update and Relaunch was pressed: the next `found` and `readyToInstall` are answered Install.
    public private(set) var pressed = false
    /// The error of a check or an install the person asked for; background errors are only logged.
    public private(set) var message: String?

    public init() {}

    /// Whether an error now concerns something the person asked for.
    private var userInitiated: Bool {
        if pressed { return true }
        switch state {
        case .checking, .downloading, .extracting, .waitingForIdle, .installing: return true
        default: return false
        }
    }

    /// The state an item found this session rests in.
    private var resting: State {
        guard item != nil else { return .idle }
        return itemIsInformationOnly ? .informationOnly : .available
    }

    public mutating func handle(_ event: Event) -> [Effect] {
        if case .unavailable = state { return [] }
        switch event {
        case .startFailed(let reason):
            state = .unavailable(reason)
            pressed = false
            return []
        case .pressed:
            switch state {
            case .idle, .available, .failed:
                pressed = true
                message = nil
                state = .checking
                return [.checkForUpdates]
            default:
                return []
            }
        case .checkStarted:
            switch state {
            case .idle, .available, .informationOnly, .failed:
                message = nil
                state = .checking
            default:
                break
            }
            return []
        case .found(let found, let informationOnly):
            item = found
            itemIsInformationOnly = informationOnly
            let record = Effect.record(found, informationOnly: informationOnly)
            if informationOnly {
                pressed = false
                state = .informationOnly
                return [record, .reply(.dismiss)]
            }
            if pressed {
                state = .downloading
                return [record, .reply(.install)]
            }
            state = .available
            return [record, .reply(.dismiss)]
        case .notFound:
            switch state {
            case .idle, .available, .informationOnly, .checking, .failed:
                item = nil
                itemIsInformationOnly = false
                pressed = false
                state = .idle
                return [.forget]
            default:
                return []
            }
        case .downloadStarted:
            if state == .checking || state == .available { state = .downloading }
            return []
        case .extracting:
            if state == .downloading { state = .extracting }
            return []
        case .readyToInstall:
            guard pressed, !itemIsInformationOnly else {
                state = resting
                return [.reply(.dismiss)]
            }
            pressed = false
            state = .installing
            return [.reply(.install)]
        case .postponeAsked(let busy):
            guard state == .installing || state == .waitingForIdle else { return [] }
            guard busy else {
                state = .installing
                return []
            }
            state = .waitingForIdle
            return [.postpone]
        case .runnerIdle:
            guard state == .waitingForIdle else { return [] }
            state = .installing
            return [.resumeInstall]
        case .installStarted:
            if state != .installingOnQuit { state = .installing }
            return []
        case .error(let text):
            let shown = userInitiated
            pressed = false
            guard shown else { return [] }
            message = text
            state = item == nil ? .failed : resting
            return []
        case .appQuits:
            guard state == .waitingForIdle else { return [] }
            state = .installingOnQuit
            return [.dropPostponed]
        }
    }
}
