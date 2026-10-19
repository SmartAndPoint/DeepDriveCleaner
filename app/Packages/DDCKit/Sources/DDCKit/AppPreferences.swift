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
    public static let deletePermanentlyKey = "DeleteReviewItemsPermanently"
    public static let scansPausedKey = "ScansPaused"

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        // The size is off by default (owner, 2026-10-19): next to the icon it reads as
        // a second total beside the popover's "Selected to clean".
        defaults.register(defaults: [Self.showSizeKey: false, Self.showBadgeKey: true, Self.deletePermanentlyKey: false])
    }

    /// "Show the size you can free next to the icon".
    public var showSize: Bool { defaults.bool(forKey: Self.showSizeKey) }
    /// "Show a badge on the menu bar icon".
    public var showBadge: Bool { defaults.bool(forKey: Self.showBadgeKey) }

    /// "Delete review items instead of moving them to the Trash": where each
    /// `review` item's Delete permanently starts in the confirmation sheet.
    public var deleteReviewPermanently: Bool { defaults.bool(forKey: Self.deletePermanentlyKey) }

    /// Pause in the menu: no scan starts until Resume, scheduled ones included.
    public var scansPaused: Bool { defaults.bool(forKey: Self.scansPausedKey) }

    public func setScansPaused(_ on: Bool) {
        defaults.set(on, forKey: Self.scansPausedKey)
    }

    /// A notification was posted and the Review window has not opened since.
    public var badgePending: Bool { defaults.bool(forKey: Self.badgePendingKey) }

    public func notificationPosted() {
        defaults.set(true, forKey: Self.badgePendingKey)
    }

    public func clearBadge() {
        defaults.removeObject(forKey: Self.badgePendingKey)
    }
}
