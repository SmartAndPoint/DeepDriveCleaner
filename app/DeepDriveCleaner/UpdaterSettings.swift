import DDCKit
import Foundation
import Observation

/// Settings → General's update rows (ADR "Updates through Sparkle, alongside
/// the Homebrew cask", §2 and §5).
///
/// The switch is Sparkle's `automaticallyChecksForUpdates`, which Sparkle keeps
/// in the app's defaults; nothing here copies it. Like `LoginItem`, the state
/// shown is re-read whenever Settings appears and when a check ends.
@MainActor
@Observable
final class UpdaterSettings {
    private(set) var checksDaily = false
    private(set) var lastCheck: Date?
    private(set) var canCheck = false
    /// The update line's model: the found update, and why the updater did not start.
    let model: UpdateModel
    /// The runner Update and Relaunch waits for.
    let activity: RunnerActivity?
    /// The running app: "0.11.0 (16)", its short version and its build.
    let version: String = {
        let info = Bundle.main.infoDictionary
        let short = info?["CFBundleShortVersionString"] as? String ?? "?"
        guard let build = info?["CFBundleVersion"] as? String else { return short }
        return "\(short) (\(build))"
    }()
    @ObservationIgnored private let updater: SparkleUpdater

    init(updater: SparkleUpdater, model: UpdateModel, activity: RunnerActivity?) {
        self.updater = updater
        self.model = model
        self.activity = activity
        refresh()
        // Sparkle's session starts and ends on its own (a background check, an
        // install): Check Now follows it, not only Settings appearing.
        updater.onCanCheckChanged = { [weak self] in self?.refresh() }
    }

    /// `SPUUpdater.start()` threw: the switch and Check Now are off, with the reason.
    var unavailable: String? {
        if case .unavailable(let reason) = model.state { return reason }
        return nil
    }

    func refresh() {
        checksDaily = updater.automaticallyChecksForUpdates
        lastCheck = updater.lastUpdateCheckDate
        canCheck = updater.canCheckForUpdates
    }

    func setChecksDaily(_ on: Bool) {
        updater.automaticallyChecksForUpdates = on
        refresh()
    }

    /// Check Now: Sparkle can start a check, and the update line has nothing under way.
    var checkNowEnabled: Bool { unavailable == nil && canCheck && model.atRest }

    /// One request, also with the daily check off.
    func checkNow() {
        updater.check()
        refresh()
    }
}
