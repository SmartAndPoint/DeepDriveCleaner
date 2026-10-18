import AppKit
import DDCKit
import UserNotifications

/// Feeds AppKit's launch, reopen and activate events and notification responses
/// to `LaunchMode`, and applies what it decides (ADR §5).
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, UNUserNotificationCenterDelegate {
    private var launchMode = LaunchMode()
    private var statusItem: StatusItemController?
    private lazy var settings = SettingsWindowController()
    private var runner: CLIRunner?

    func applicationWillFinishLaunching(_ notification: Notification) {
        // Before launch ends, so a notification response that launched the app arrives.
        UNUserNotificationCenter.current().delegate = self
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "?"
        AppLog.write("launch \(version) \(Bundle.main.bundlePath) arguments: \(CommandLine.arguments.dropFirst())")
        do {
            let cli = try HelperLocator.cli(bundleURL: Bundle.main.bundleURL, mainExecutable: Bundle.main.executableURL)
            runner = CLIRunner(executable: cli)
        } catch {
            AppLog.write("the embedded CLI is unavailable: \(error)")
        }
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
        for effect in effects {
            switch effect {
            case .showStatusItem:
                guard statusItem == nil else { continue }
                // `open -j` launches the scheduled instance hidden.
                if NSApp.isHidden { NSApp.unhideWithoutActivation() }
                statusItem = StatusItemController(
                    scanNow: { [weak self] in self?.scanNow() },
                    openSettings: { [weak self] in self?.settings.show() }
                )
                AppLog.write("status item shown")
            case .startScheduledWork:
                Task {
                    await scan(reason: "scheduled")
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

    // MARK: - Scans

    func scanNow() {
        Task { await scan(reason: "Scan now") }
    }

    private func scan(reason: String) async {
        guard let runner else {
            AppLog.write("scan (\(reason)) skipped: no embedded CLI")
            return
        }
        AppLog.write("scan (\(reason)) started")
        let result = await runner.run(.scan, ["scan", "--json"])
        guard result.outcome == .done else {
            AppLog.write("scan (\(reason)) ended \(result.outcome); stderr: \(result.stderrTail.suffix(5).joined(separator: " | "))")
            return
        }
        do {
            let report = try ScanReport.read(result.stdout)
            AppLog.write(
                "scan (\(reason)) done: full_disk_access=\(report.fullDiskAccess), \(report.totals.files) files in \(report.durationMs) ms, "
                    + "\(report.findings.count) findings, \(report.errorsTotal) unreadable"
            )
        } catch {
            AppLog.write("scan (\(reason)) report unreadable: \(error)")
        }
    }
}
