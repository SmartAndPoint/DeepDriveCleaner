import Foundation

/// The app's own preferences, in `UserDefaults`: never in `settings.json`,
/// which the CLI shares and which refuses unknown keys (ADR §6). The Settings
/// window's General, Schedule and Automation panes write them.
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
    public static let scheduleEnabledKey = "ScheduleEnabled"
    public static let scheduleRepeatKey = "ScheduleRepeat"
    public static let scheduleWeekdayKey = "ScheduleWeekday"
    public static let scheduleHourKey = "ScheduleHour"
    public static let afterScheduledScanKey = "AfterScheduledScan"
    public static let workingHoursOnlyKey = "NotifyWorkingHoursOnly"
    public static let notReadableKey = "NotReadableFolders"

    private let defaults: UserDefaults

    public init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
        // The size is off by default (owner, 2026-10-19): next to the icon it reads as
        // a second total beside the popover's "Selected to clean".
        // Scan automatically is on, daily at 10:00 (decision D1).
        let schedule = Schedule()
        defaults.register(defaults: [
            Self.showSizeKey: false, Self.showBadgeKey: true, Self.deletePermanentlyKey: false,
            Self.scheduleEnabledKey: schedule.enabled, Self.scheduleRepeatKey: schedule.repeats.rawValue,
            Self.scheduleWeekdayKey: schedule.weekday, Self.scheduleHourKey: schedule.hour,
            Self.afterScheduledScanKey: AfterScheduledScan.notify.rawValue, Self.workingHoursOnlyKey: false,
        ])
    }

    /// "Show the size you can free next to the icon".
    public var showSize: Bool { defaults.bool(forKey: Self.showSizeKey) }
    /// "Show a badge on the menu bar icon".
    public var showBadge: Bool { defaults.bool(forKey: Self.showBadgeKey) }

    public func setShowSize(_ on: Bool) {
        defaults.set(on, forKey: Self.showSizeKey)
    }

    public func setShowBadge(_ on: Bool) {
        defaults.set(on, forKey: Self.showBadgeKey)
    }

    /// "Delete review items instead of moving them to the Trash": where each
    /// `review` item's Delete permanently starts in the confirmation sheet.
    public var deleteReviewPermanently: Bool { defaults.bool(forKey: Self.deletePermanentlyKey) }

    public func setDeleteReviewPermanently(_ on: Bool) {
        defaults.set(on, forKey: Self.deletePermanentlyKey)
    }

    /// "Scan automatically" and its slot; a stored value out of range reads as the default's.
    public var schedule: Schedule {
        Schedule(
            enabled: defaults.bool(forKey: Self.scheduleEnabledKey),
            repeats: defaults.string(forKey: Self.scheduleRepeatKey).flatMap(Schedule.Repeat.init) ?? .daily,
            weekday: defaults.integer(forKey: Self.scheduleWeekdayKey),
            hour: defaults.integer(forKey: Self.scheduleHourKey)
        )
    }

    public func setSchedule(_ schedule: Schedule) {
        defaults.set(schedule.enabled, forKey: Self.scheduleEnabledKey)
        defaults.set(schedule.repeats.rawValue, forKey: Self.scheduleRepeatKey)
        defaults.set(schedule.weekday, forKey: Self.scheduleWeekdayKey)
        defaults.set(schedule.hour, forKey: Self.scheduleHourKey)
    }

    /// "After a scheduled scan"; an unknown stored value reads as Notify me.
    public var afterScheduledScan: AfterScheduledScan {
        defaults.string(forKey: Self.afterScheduledScanKey).flatMap(AfterScheduledScan.init) ?? .notify
    }

    public func setAfterScheduledScan(_ choice: AfterScheduledScan) {
        defaults.set(choice.rawValue, forKey: Self.afterScheduledScanKey)
    }

    /// "Only during working hours": 9:00 to 19:00 on weekdays.
    public var workingHoursOnly: Bool { defaults.bool(forKey: Self.workingHoursOnlyKey) }

    public func setWorkingHoursOnly(_ on: Bool) {
        defaults.set(on, forKey: Self.workingHoursOnlyKey)
    }

    /// The prompting areas the app's last Scan now could not read, as
    /// absolute paths (`NotReadable`). Never from a report read from disk.
    public var notReadable: [String] { defaults.stringArray(forKey: Self.notReadableKey) ?? [] }

    public func setNotReadable(_ paths: [String]) {
        defaults.set(paths, forKey: Self.notReadableKey)
    }

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
