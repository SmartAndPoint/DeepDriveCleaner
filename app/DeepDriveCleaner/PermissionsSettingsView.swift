import DDCKit
import Observation
import SwiftUI
import UserNotifications

/// Settings → Permissions (artboard 10): Full Disk Access from the
/// popover's probe, the notification permission as macOS has it, and the
/// scheduler's rows once "Background scan and notifications in the app"
/// fills their seams. Settings never asks for a permission.
struct PermissionsSettingsView: View {
    let model: SettingsModel
    let notifications: NotificationPermission
    let loginItem: LoginItem
    let privacy: PrivacySettings

    var body: some View {
        Form {
            Section {
                LabeledContent {
                    Button("Open Privacy Settings") { privacy.openFullDiskAccess() }
                } label: {
                    Text(model.fullDiskAccess ? "Full Disk Access · granted" : "Full Disk Access · off")
                    Text("Lets every scan read every folder, ~/Library included, without asking folder by folder. It belongs to the app, so updates keep it.")
                }
            }
            Section {
                LabeledContent {
                    EmptyView()
                } label: {
                    Text("Notifications · \(notifications.state)")
                    Text("Banners with Review and Remind me tomorrow.")
                }
                if let registration = model.registration {
                    LabeledContent {
                        if registration == .requiresApproval {
                            Button("Open Login Items…") { loginItem.openLoginItemsSettings() }
                        }
                    } label: {
                        Text("Scheduled scan · \(Self.words(registration))")
                        Text("Listed under DeepDriveCleaner in Login Items & Extensions. If macOS waits for your approval, this row says so and opens it.")
                    }
                }
                if model.legacyAgentFound {
                    LabeledContent {
                        Button("Replace It") { Task { await model.replaceLegacyAgent() } }
                    } label: {
                        Text("Old background scan found")
                        Text("The command-line scan from ddc schedule install still runs. The app now scans on its own.")
                    }
                    if let failure = model.replaceFailure {
                        Label(failure, systemImage: "exclamationmark.triangle")
                            .foregroundStyle(ReportClassStyle.ink(.review))
                    }
                }
            }
        }
        .settingsPane()
    }

    private static func words(_ registration: SchedulerRegistration) -> String {
        switch registration {
        case .allowed: "allowed"
        case .requiresApproval: "waiting for your approval"
        case .off: "off"
        }
    }
}

/// The app's notification permission, read and never requested: the first
/// scheduled notification asks ("Background scan and notifications in the app").
@MainActor
@Observable
final class NotificationPermission {
    private(set) var status: UNAuthorizationStatus = .notDetermined

    var state: String {
        switch status {
        case .authorized, .provisional, .ephemeral: "allowed"
        case .denied: "off"
        case .notDetermined: "not asked yet"
        @unknown default: "unknown"
        }
    }

    func refresh() async {
        status = await UNUserNotificationCenter.current().notificationSettings().authorizationStatus
    }
}
