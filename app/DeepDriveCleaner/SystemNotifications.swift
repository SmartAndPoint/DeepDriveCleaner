import DDCKit
import UserNotifications

/// `ScheduledWork`'s port to `UNUserNotificationCenter` (artboard 2): the
/// `scan-result` category with Review and Remind Me Tomorrow, and one fixed
/// request id, so a newer notification replaces the one before. DDCKit
/// never imports UserNotifications; it crashes outside a bundle.
@MainActor
final class SystemNotifications: NotificationPosting {
    static let category = "scan-result"
    static let reviewAction = "review"
    static let remindAction = "remind-tomorrow"
    static let requestID = "scheduled-scan"

    /// In `applicationWillFinishLaunching`, before a response that launched the app arrives.
    static func registerCategory() {
        let review = UNNotificationAction(identifier: reviewAction, title: "Review", options: [.foreground])
        let remind = UNNotificationAction(identifier: remindAction, title: "Remind Me Tomorrow", options: [])
        let category = UNNotificationCategory(identifier: category, actions: [review, remind], intentIdentifiers: [])
        UNUserNotificationCenter.current().setNotificationCategories([category])
    }

    func authorization() async -> NotificationAuthorization {
        switch await UNUserNotificationCenter.current().notificationSettings().authorizationStatus {
        case .authorized, .provisional, .ephemeral: .authorized
        case .denied: .denied
        case .notDetermined: .notDetermined
        @unknown default: .denied
        }
    }

    func requestAuthorization() async -> Bool {
        do {
            return try await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound])
        } catch {
            AppLog.write("notification permission request failed: \(error)")
            return false
        }
    }

    func post(_ text: NotificationText) async -> Bool {
        let content = UNMutableNotificationContent()
        content.title = text.title
        content.body = text.body
        content.categoryIdentifier = Self.category
        do {
            try await UNUserNotificationCenter.current().add(UNNotificationRequest(identifier: Self.requestID, content: content, trigger: nil))
            return true
        } catch {
            AppLog.write("notification not posted: \(error)")
            return false
        }
    }
}
