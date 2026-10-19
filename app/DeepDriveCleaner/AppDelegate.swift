import AppKit
import DDCKit
import UserNotifications

/// Feeds AppKit's launch, reopen and activate events and notification responses
/// to `LaunchMode`, and applies what it decides (ADR §5).
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, UNUserNotificationCenterDelegate {
    /// The per-user TCC database: it opens only with Full Disk Access, which is
    /// how the app and the CLI both tell. Built here only, never in DDCKit, so
    /// no test ever touches it.
    static let tccDatabase = FileManager.default.homeDirectoryForCurrentUser
        .appending(path: "Library/Application Support/com.apple.TCC/TCC.db")

    private var launchMode = LaunchMode()
    private var statusItem: StatusItemController?
    private lazy var settings = SettingsWindowController()
    private let router = ReviewRouterStub()
    private var model: PopoverModel?

    func applicationWillFinishLaunching(_ notification: Notification) {
        // Before launch ends, so a notification response that launched the app arrives.
        UNUserNotificationCenter.current().delegate = self
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        AppLog.write("launch \(version ?? "?") \(Bundle.main.bundlePath) arguments: \(CommandLine.arguments.dropFirst())")
        var runner: CLIRunner?
        do {
            let cli = try HelperLocator.cli(bundleURL: Bundle.main.bundleURL, mainExecutable: Bundle.main.executableURL)
            runner = CLIRunner(executable: cli)
        } catch {
            AppLog.write("the embedded CLI is unavailable: \(error)")
        }
        // In every launch mode, so a promoted scheduled instance shows its running scan.
        model = PopoverModel(
            runner: runner,
            data: .current(),
            probe: FullDiskAccessProbe(path: Self.tccDatabase),
            preferences: AppPreferences(),
            router: router,
            // The due function arrives with "Background scan and notifications in the app".
            nextScan: { nil },
            // The build script keeps the app's version equal to its CLI's.
            appVersion: version,
            log: { AppLog.write($0) }
        )
        apply(launchMode.handle(.launched(arguments: CommandLine.arguments)))
    }

    func applicationWillTerminate(_ notification: Notification) {
        // The last lines of a scheduled instance are the spike's evidence.
        AppLog.flush()
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        AppLog.write("reopen in \(launchMode.mode) mode")
        apply(launchMode.handle(.reopen))
        return false
    }

    func applicationDidBecomeActive(_ notification: Notification) {
        AppLog.write("activate in \(launchMode.mode) mode")
        apply(launchMode.handle(.activate))
        if let model { Task { await model.refresh() } }
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse) async {
        await MainActor.run {
            AppLog.write("notification response in \(launchMode.mode) mode")
            apply(launchMode.handle(.notificationResponse))
        }
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .list]
    }

    // MARK: - Effects

    private func apply(_ effects: [LaunchMode.Effect]) {
        guard let model else { return }
        for effect in effects {
            switch effect {
            case .showStatusItem:
                guard statusItem == nil else { continue }
                // `open -j` launches the scheduled instance hidden.
                if NSApp.isHidden { NSApp.unhideWithoutActivation() }
                statusItem = StatusItemController(
                    model: model,
                    openSettings: { [weak self] in self?.settings.show() },
                    openPrivacy: { Self.openFullDiskAccessSettings() }
                )
                AppLog.write("status item shown")
                Task { await model.start() }
            case .startScheduledWork:
                // Unconditional until the due function of "Background scan and
                // notifications in the app" decides whether a scan is due.
                Task {
                    await model.scanNow()
                    apply(launchMode.handle(.workFinished))
                }
            case .scanIfDue:
                // The due function arrives with "Background scan and notifications in the app".
                AppLog.write("reopen of the running app: scan if due")
            case .quit:
                Task { await quitAfterScheduledWork() }
            }
        }
    }

    private func quitAfterScheduledWork() async {
        #if DEBUG
        // The spike's promotion check needs a scheduled instance that outlives
        // a scan of a near-empty home: `defaults write com.smartandpoint.deepdrivecleaner DebugHoldSeconds 120`.
        let hold = UserDefaults.standard.double(forKey: "DebugHoldSeconds")
        if hold > 0 {
            AppLog.write("DebugHoldSeconds: holding \(hold) s before quitting")
            try? await Task.sleep(for: .seconds(hold))
        }
        #endif
        // A user's event handled since the work ended has promoted the instance.
        guard launchMode.mode == .scheduled else {
            AppLog.write("promoted before quitting: keeps running")
            return
        }
        AppLog.write("scheduled work done: quitting")
        NSApp.terminate(nil)
    }

    // MARK: - System Settings

    /// Privacy & Security → Full Disk Access. The anchor's form changed with
    /// System Settings; the manual check on macOS 26 tells which one it honours.
    static func openFullDiskAccessSettings() {
        let candidates = [
            "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles",
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles",
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension",
        ]
        for candidate in candidates {
            if let url = URL(string: candidate), NSWorkspace.shared.open(url) {
                AppLog.write("opened \(candidate)")
                return
            }
        }
        AppLog.write("could not open the Full Disk Access settings")
    }
}

/// The review seam until "Review and clean window" builds the window: it
/// opens nothing, so the popover says the window arrives in a later build.
@MainActor
final class ReviewRouterStub: ReviewRouting {
    func open(_ request: ReviewRequest) -> Bool {
        AppLog.write("review requested (group \(request.group ?? "all"), confirm \(request.confirm)): no Review window yet")
        return false
    }
}
