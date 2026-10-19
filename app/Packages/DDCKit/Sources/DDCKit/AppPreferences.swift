import Foundation

/// The app's own preferences, in `UserDefaults`: never in `settings.json`,
/// which the CLI shares and which refuses unknown keys (ADR §6). The Settings
/// story adds the switches; until then `defaults write
/// com.smartandpoint.deepdrivecleaner ShowSizeNextToIcon -bool true` works.
///
/// The attention badge is kept here too: a `--scheduled` instance posts a
/// notification and quits, and the badge must outlive it.
@MainActor
public final class AppPreferences {
    public static let showSizeKey = "ShowSizeNextToIcon"
    public static let showBadgeKey = "ShowMenuBarBadge"
    public static let badgePendingKey = "AttentionBadgePending"

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        // The size is off by default (owner, 2026-10-19): next to the icon it reads as
        // a second total beside the popover's "Selected to clean".
        defaults.register(defaults: [Self.showSizeKey: false, Self.showBadgeKey: true])
    }

    /// "Show the size you can free next to the icon".
    public var showSize: Bool { defaults.bool(forKey: Self.showSizeKey) }
    /// "Show a badge on the menu bar icon".
    public var showBadge: Bool { defaults.bool(forKey: Self.showBadgeKey) }

    /// A notification was posted and the Review window has not opened since.
    public var badgePending: Bool { defaults.bool(forKey: Self.badgePendingKey) }

    public func notificationPosted() {
        defaults.set(true, forKey: Self.badgePendingKey)
    }

    public func clearBadge() {
        defaults.removeObject(forKey: Self.badgePendingKey)
    }
}
