import AppKit
import DDCKit
import Sparkle

/// Sparkle's user driver and its updater delegate (ADR "Updates through
/// Sparkle, alongside the Homebrew cask", §2 and §3): every call becomes an
/// `UpdateModel` event, and Sparkle gets the reply the flow decides. Sparkle
/// opens no window of its own while the app runs.
///
/// Only the completion-handler forms of Sparkle's calls are implemented, and
/// each completion is called once. A found update is answered Dismiss unless
/// Update and Relaunch was pressed; the relaunch is held while the runner has
/// work. The updater is created at launch and started only on `LaunchMode`'s
/// `.startUpdater`.
@MainActor
final class SparkleUpdater: NSObject, SPUUserDriver, SPUUpdaterDelegate {
    /// No version, so GitHub learns nothing about this copy from it (ADR §10).
    static let userAgent = "DeepDriveCleaner"

    private let model: UpdateModel
    private var updater: SPUUpdater?
    /// The download's size, once Sparkle knows it, and what has arrived.
    private var expectedBytes: UInt64 = 0
    private var receivedBytes: UInt64 = 0
    /// Called when a check ends, so Settings shows the new last check time.
    var onCycleFinished: (@MainActor () -> Void)?

    init(model: UpdateModel) {
        self.model = model
        super.init()
        let updater = SPUUpdater(hostBundle: .main, applicationBundle: .main, userDriver: self, delegate: self)
        updater.userAgentString = Self.userAgent
        self.updater = updater
        model.startCheck = { [weak self] in self?.check() }
    }

    /// The feed this build reads: the Debug feed in a Debug build, else `SUFeedURL`.
    static var feedURL: URL {
        #if DEBUG
        return debugFeedURL
        #else
        // build-app.sh --dist refuses a release without the production feed.
        let feed = Bundle.main.object(forInfoDictionaryKey: "SUFeedURL") as? String
        return feed.flatMap(URL.init(string:)) ?? URL(string: "about:blank")!
        #endif
    }

    #if DEBUG
    /// A Debug build never reads the production feed: it reads
    /// scripts/debug-feed.sh's, served on 127.0.0.1 at
    /// `defaults write com.smartandpoint.deepdrivecleaner DebugFeedPort 8000`
    /// (8000, `python3 -m http.server`'s port, when unset).
    static var debugFeedURL: URL {
        let port = UserDefaults.standard.integer(forKey: "DebugFeedPort")
        return URL(string: "http://127.0.0.1:\(port > 0 ? port : 8000)/appcast.xml")!
    }

    func feedURLString(for updater: SPUUpdater) -> String? {
        Self.debugFeedURL.absoluteString
    }
    #endif

    /// `SPUUpdater.start()`, on `.startUpdater`. A throw leaves the rest of the
    /// app as it was: no update line, and Settings says why.
    func start() throws {
        guard let updater else { return }
        do {
            try updater.start()
            AppLog.write("updater: started, feed \(Self.feedURL.absoluteString), daily checks \(updater.automaticallyChecksForUpdates ? "on" : "off")")
        } catch {
            AppLog.write("updater: did not start: \(error)")
            throw error
        }
    }

    // MARK: Settings

    /// "Check for updates daily", stored by Sparkle in the app's defaults (ADR §5).
    var automaticallyChecksForUpdates: Bool {
        get { updater?.automaticallyChecksForUpdates ?? false }
        set {
            updater?.automaticallyChecksForUpdates = newValue
            AppLog.write("updater: daily checks \(newValue ? "on" : "off")")
        }
    }

    var lastUpdateCheckDate: Date? { updater?.lastUpdateCheckDate }

    /// False before the start, after a failed one, and while a check runs.
    var canCheckForUpdates: Bool { updater?.canCheckForUpdates ?? false }

    /// Check Now, and the check Update and Relaunch starts.
    func check() {
        guard let updater, updater.canCheckForUpdates else {
            // Update and Relaunch waits for a check that would never come.
            model.send(.error("Another check is running. Try again in a moment."))
            return
        }
        AppLog.write("updater: a check you asked for")
        updater.checkForUpdates()
    }

    // MARK: SPUUserDriver

    func show(_ request: SPUUpdatePermissionRequest, reply: @escaping (SUUpdatePermissionResponse) -> Void) {
        // SUEnableAutomaticChecks is set, so Sparkle never asks; the answer is the
        // owner's default, with no system profile.
        reply(SUUpdatePermissionResponse(automaticUpdateChecks: true, sendSystemProfile: false))
    }

    func showUserInitiatedUpdateCheck(cancellation: @escaping () -> Void) {
        model.send(.checkStarted)
    }

    func showUpdateFound(with appcastItem: SUAppcastItem, state: SPUUserUpdateState, reply: @escaping (SPUUserUpdateChoice) -> Void) {
        let item = UpdateFlow.Item(
            version: appcastItem.versionString,
            shortVersion: appcastItem.displayVersionString,
            releaseURL: appcastItem.fullReleaseNotesURL ?? appcastItem.infoURL
        )
        let informationOnly = appcastItem.isInformationOnlyUpdate
        AppLog.write("updater: found \(item.shortVersion) (\(item.version))\(informationOnly ? ", information only" : "")")
        reply(choice(model.send(.found(item, informationOnly: informationOnly))))
    }

    func showUpdateReleaseNotes(with downloadData: SPUDownloadData) {
        // The appcast names no release notes to fetch: the line links the release page.
    }

    func showUpdateReleaseNotesFailedToDownloadWithError(_ error: any Error) {}

    func showUpdateNotFoundWithError(_ error: any Error, acknowledgement: @escaping () -> Void) {
        model.send(.notFound)
        acknowledgement()
    }

    func showUpdaterError(_ error: any Error, acknowledgement: @escaping () -> Void) {
        AppLog.write("updater: error: \(error)")
        model.send(.error(error.localizedDescription))
        acknowledgement()
    }

    func showDownloadInitiated(cancellation: @escaping () -> Void) {
        expectedBytes = 0
        receivedBytes = 0
        model.send(.downloadStarted)
    }

    func showDownloadDidReceiveExpectedContentLength(_ expectedContentLength: UInt64) {
        expectedBytes = expectedContentLength
        receivedBytes = 0
        model.setProgress(0)
    }

    func showDownloadDidReceiveData(ofLength length: UInt64) {
        receivedBytes += length
        guard expectedBytes > 0 else { return }
        model.setProgress(Double(receivedBytes) / Double(expectedBytes))
    }

    func showDownloadDidStartExtractingUpdate() {
        model.send(.extracting)
    }

    func showExtractionReceivedProgress(_ progress: Double) {
        model.setProgress(progress)
    }

    func showReady(toInstallAndRelaunch reply: @escaping (SPUUserUpdateChoice) -> Void) {
        reply(choice(model.send(.readyToInstall)))
    }

    func showInstallingUpdate(withApplicationTerminated applicationTerminated: Bool, retryTerminatingApplication: @escaping () -> Void) {
        AppLog.write("updater: installing, the app \(applicationTerminated ? "has quit" : "quits next")")
        model.send(.installStarted)
    }

    func showUpdateInstalledAndRelaunched(_ relaunched: Bool, acknowledgement: @escaping () -> Void) {
        acknowledgement()
    }

    func dismissUpdateInstallation() {
        // The session ended; the line keeps what the flow knows.
    }

    // MARK: SPUUpdaterDelegate

    func updaterDidNotFindUpdate(_ updater: SPUUpdater, error: any Error) {
        // Also after a background check: the kept update is installed or withdrawn.
        model.send(.notFound)
    }

    func updater(_ updater: SPUUpdater, shouldPostponeRelaunchForUpdate item: SUAppcastItem, untilInvokingBlock installHandler: @escaping () -> Void) -> Bool {
        model.shouldPostpone { installHandler() }
    }

    func updater(_ updater: SPUUpdater, didAbortWithError error: any Error) {
        // Background errors (no network, a GitHub outage) are logged, never shown.
        let error = error as NSError
        if error.domain == SUSparkleErrorDomain, error.code == Int(SUError.noUpdateError.rawValue) { return }
        AppLog.write("updater: aborted: \(error)")
    }

    func updater(_ updater: SPUUpdater, didFinishUpdateCycleFor updateCheck: SPUUpdateCheck, error: (any Error)?) {
        onCycleFinished?()
    }

    // MARK: -

    /// The flow's reply to Sparkle; Dismiss when it gives none.
    private func choice(_ effects: [UpdateFlow.Effect]) -> SPUUserUpdateChoice {
        effects.contains(.reply(.install)) ? .install : .dismiss
    }
}
