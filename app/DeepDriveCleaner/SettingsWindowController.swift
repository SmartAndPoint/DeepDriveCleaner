import AppKit
import SwiftUI

/// The Settings window; the story "Settings: folders, types, notifications"
/// adds its tabs. For now it holds the open-at-login toggle.
@MainActor
final class SettingsWindowController {
    private let loginItem = LoginItem()
    private var window: NSWindow?

    func show() {
        if window == nil {
            let window = NSWindow(contentViewController: NSHostingController(rootView: GeneralSettingsView(loginItem: loginItem)))
            window.title = "DeepDriveCleaner Settings"
            window.styleMask = [.titled, .closable]
            window.isReleasedWhenClosed = false
            window.center()
            self.window = window
        }
        loginItem.refresh()
        NSApp.activate()
        window?.makeKeyAndOrderFront(nil)
    }
}

struct GeneralSettingsView: View {
    let loginItem: LoginItem

    var body: some View {
        Form {
            Toggle("Open at login", isOn: Binding(get: { loginItem.isOn }, set: { loginItem.set($0) }))
            if loginItem.status == .requiresApproval {
                LabeledContent("Waiting for your approval in System Settings.") {
                    Button("Open Login Items…") { loginItem.openLoginItemsSettings() }
                }
            }
            if let failure = loginItem.failure {
                Text(failure).foregroundStyle(.red)
            }
        }
        .formStyle(.grouped)
        .frame(width: 440)
        .fixedSize(horizontal: false, vertical: true)
        .onAppear { loginItem.refresh() }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            loginItem.refresh()
        }
    }
}
