#if DEBUG
import AppKit
import ServiceManagement
import UserNotifications

/// The spike's tools ("App skeleton and build pipeline"), in Debug builds only.
/// Every result goes to `app.log`, where the owner reads the checks.
@MainActor
enum DebugMenu {
    static let agentPlist = "com.smartandpoint.deepdrivecleaner.scheduler.plist"

    static func item() -> NSMenuItem {
        let menu = NSMenu()
        menu.addItem(ClosureMenuItem("Register Scheduler Agent", handler: registerAgent))
        menu.addItem(ClosureMenuItem("Unregister Scheduler Agent", handler: unregisterAgent))
        menu.addItem(ClosureMenuItem("Probe Full Disk Access", handler: probeFullDiskAccess))
        menu.addItem(ClosureMenuItem("Post Test Notification", handler: postTestNotification))
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

    /// Opens the per-user TCC database read-only, as the CLI's `full_disk_access`
    /// does, and logs the errno: `EPERM` means no access, `ENOENT` means no file,
    /// and then a `false` from the CLI proves nothing about attribution.
    private static func probeFullDiskAccess() {
        let path = FileManager.default.homeDirectoryForCurrentUser
            .appending(path: "Library/Application Support/com.apple.TCC/TCC.db").path
        let fd = open(path, O_RDONLY)
        if fd >= 0 {
            close(fd)
            AppLog.write("probe: TCC.db opened: Full Disk Access is granted to this app")
        } else {
            let code = errno
            AppLog.write("probe: TCC.db not opened: errno \(code) (\(String(cString: strerror(code))))")
        }
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
