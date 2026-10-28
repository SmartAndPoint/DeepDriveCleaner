import Foundation

/// A scheduled scan's notification (artboard 2 of the canvas): the amount
/// and at most three groups, or the volume that is almost full. It reports;
/// it never offers to clean (ADR §7). Its actions are Review and Remind Me
/// Tomorrow.
public struct NotificationText: Codable, Sendable, Equatable {
    public let title: String
    public let body: String

    public init(title: String, body: String) {
        self.title = title
        self.body = body
    }

    /// The groups named are the summary's biggest by default-selected size.
    public static func make(_ decision: NotifyDecision, groups: [String]) -> NotificationText {
        let amount = ByteSize.human(decision.reclaimable)
        let named = Array(groups.prefix(3))
        if decision.reason == .lowDisk, let volume = decision.volume {
            let disk = Wording.volume(volume)
            let title: String
            if let free = decision.free, let total = decision.total, total > 0 {
                let full = Int((Double(total - min(free, total)) / Double(total) * 100).rounded(.down))
                title = "\(disk) is \(full)% full"
            } else {
                title = "\(disk) is almost full"
            }
            let free = decision.free.map { "\(ByteSize.human($0)) free. " } ?? ""
            let what = named.isEmpty ? "of caches and idle project folders " : "in \(Wording.list(named)) "
            return NotificationText(title: title, body: "\(free)\(amount) \(what)can be freed.")
        }
        let what = named.isEmpty ? "Caches and idle project folders" : Wording.list(named)
        return NotificationText(title: "\(amount) can be freed", body: "\(what). Nothing is removed until you review them.")
    }
}

/// A decision the app posts later: outside working hours, it waits for the
/// first tick inside them (`--scheduled` posts it without scanning).
public struct PendingNotification: Codable, Sendable, Equatable {
    public let text: NotificationText
    /// What `notify ack --bytes` records once it is posted.
    public let bytes: UInt64
    public let decided: Date

    public init(text: NotificationText, bytes: UInt64, decided: Date) {
        self.text = text
        self.bytes = bytes
        self.decided = decided
    }
}

public enum NotificationAuthorization: Sendable, Equatable {
    case authorized, denied, notDetermined
}

/// The app's notifications (`UNUserNotificationCenter` in the app target;
/// DDCKit never imports it, and tests use a fake).
@MainActor
public protocol NotificationPosting: AnyObject {
    func authorization() async -> NotificationAuthorization
    /// Shows macOS's question: never from a scheduled run.
    func requestAuthorization() async -> Bool
    /// True once macOS accepted the notification.
    func post(_ text: NotificationText) async -> Bool
}
