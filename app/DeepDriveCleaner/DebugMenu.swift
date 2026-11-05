#if DEBUG
import AppKit
import DDCKit
import ServiceManagement
import UserNotifications

/// The spike's tools ("App skeleton and build pipeline"), in Debug builds only.
/// Every result goes to `app.log`, where the owner reads the checks.
@MainActor
enum DebugMenu {
    static let agentPlist = SchedulerAgent.primaryPlist

    static func item(model: PopoverModel) -> NSMenuItem {
        let menu = NSMenu()
        menu.addItem(ClosureMenuItem("Register Scheduler Agent", handler: registerAgent))
        menu.addItem(ClosureMenuItem("Unregister Scheduler Agent", handler: unregisterAgent))
        menu.addItem(ClosureMenuItem("Probe Full Disk Access", handler: probeFullDiskAccess))
        menu.addItem(ClosureMenuItem("Post Test Notification", handler: postTestNotification))
        // The badge has no other trigger until "Background scan and notifications in the app".
        menu.addItem(ClosureMenuItem("Set Attention Badge") { model.notificationPosted() })
        menu.addItem(ClosureMenuItem("Clear Attention Badge") {
            AppPreferences().clearBadge()
            Task { await model.refresh() }
        })
        menu.addItem(ClosureMenuItem("Show app.log in Finder") { NSWorkspace.shared.activateFileViewerSelecting([AppLog.url]) })
        let item = NSMenuItem(title: "Debug", action: nil, keyEquivalent: "")
        item.submenu = menu
        return item
    }

    private static func registerAgent() {
        let agent = SMAppService.agent(plistName: agentPlist)
        do {
            try agent.register()
            AppLog.write("agent register: \(agent.status.name)")
        } catch {
            AppLog.write("agent register failed: \(error); status \(agent.status.name)")
        }
    }

    private static func unregisterAgent() {
        let agent = SMAppService.agent(plistName: agentPlist)
        Task {
            do {
                try await agent.unregister()
                AppLog.write("agent unregister: \(agent.status.name)")
            } catch {
                AppLog.write("agent unregister failed: \(error); status \(agent.status.name)")
            }
        }
    }

    /// The popover's probe, one line per step with its errno: `EPERM` means
    /// no access, `ENOENT` no such file or folder; either sends the probe to
    /// the next step. When every step says `ENOENT`, a `false` from the CLI
    /// proves nothing about attribution.
    private static func probeFullDiskAccess() {
        let steps = AppDelegate.fullDiskAccessProbe.steps()
        for step in steps {
            let name = step.location.url.lastPathComponent
            if let code = step.errno {
                AppLog.write("probe: \(name) not read: errno \(code) (\(String(cString: strerror(code))))")
            } else {
                AppLog.write("probe: \(name) read")
            }
        }
        AppLog.write(steps.last.map { $0.errno == nil } == true
            ? "probe: Full Disk Access is granted to this app"
            : "probe: Full Disk Access is not granted to this app")
    }

    private static func postTestNotification() {
        Task {
            let center = UNUserNotificationCenter.current()
            do {
                let granted = try await center.requestAuthorization(options: [.alert, .sound])
                AppLog.write("notifications authorised: \(granted)")
                let content = UNMutableNotificationContent()
                content.title = "DeepDriveCleaner"
                content.body = "Test notification from the Debug menu."
                try await center.add(UNNotificationRequest(identifier: "debug-test", content: content, trigger: nil))
                AppLog.write("test notification posted")
            } catch {
                AppLog.write("test notification failed: \(error)")
            }
        }
    }
}
#endif
