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
    /// In the Dock while a window is open (menu bar ADR §2, amended for 0.2.1).
    private let presence = DockPresenceController()
    private lazy var settings = SettingsWindowController(presence: presence)
    private lazy var reviewWindow = ReviewWindowController(presence: presence)
    private var review: ReviewModel?
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
        let data = DataDirectory.current()
        let preferences = AppPreferences()
        // The window opens on what the popover shows, and its clean turns the
        // popover's icon; the popover is its router.
        let review = ReviewModel(
            runner: runner,
            data: data,
            preferences: preferences,
            snapshot: { [weak self] in self?.model?.snapshot },
            present: { [weak self] in
                guard let self, let review = self.review else { return false }
                return self.reviewWindow.show(review)
            },
            setBusy: { [weak self] in self?.model?.setCleaning($0) },
            rescan: { [weak self] in await self?.model?.scanAfterClean() },
            paused: { [weak self] in self?.model?.paused ?? false },
            log: { AppLog.write($0) }
        )
        self.review = review
        // In every launch mode, so a promoted scheduled instance shows its running scan.
        model = PopoverModel(
            runner: runner,
            data: data,
            probe: FullDiskAccessProbe(path: Self.tccDatabase),
            preferences: preferences,
            router: review,
            // The due function arrives with "Background scan and notifications in the app".
            nextScan: { nil },
            // The build script keeps the app's version equal to its CLI's.
            appVersion: version,
            log: { AppLog.write($0) }
        )
        NSApp.mainMenu = MainMenu.make(
            about: { [weak self] in self?.presence.showAbout() },
            settings: { [weak self] in self?.settings.show() }
        )
        apply(launchMode.handle(.launched(arguments: CommandLine.arguments)))
    }

    /// Quit during a clean stops it after the current item, then quits: the
    /// CLI is a child process and would go on cleaning unwatched.
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard let review, review.prepareToQuit({ NSApp.reply(toApplicationShouldTerminate: true) }) else { return .terminateNow }
        AppLog.write("quit during a clean: stopping it after the current item")
        return .terminateLater
    }

    func applicationWillTerminate(_ notification: Notification) {
        // The last lines of a scheduled instance are the spike's evidence.
        AppLog.flush()
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        AppLog.write("reopen in \(launchMode.mode) mode")
        apply(launchMode.handle(.reopen(windowOpen: presence.isRegular)))
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
                let statusItem = StatusItemController(
                    model: model,
                    openSettings: { [weak self] in self?.settings.show() },
                    openPrivacy: { Self.openFullDiskAccessSettings() }
                )
                self.statusItem = statusItem
                reviewWindow.returnToPopover = { [weak statusItem] in statusItem?.showPopover() }
                AppLog.write("status item shown")
                Task { await model.start() }
            case .startScheduledWork:
                // Skipped while paused (inside `scanNow`); otherwise unconditional
                // until the due function of "Background scan and notifications
                // in the app" decides whether a scan is due.
                Task {
                    await model.scanNow()
                    apply(launchMode.handle(.workFinished))
                }
            case .scanIfDue:
                // The due function arrives with "Background scan and notifications in the app".
                AppLog.write("reopen of the running app: scan if due")
            case .frontWindows:
                presence.frontAll()
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
