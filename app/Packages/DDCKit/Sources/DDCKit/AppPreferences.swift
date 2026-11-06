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
    public static let lastScheduledScanKey = "LastScheduledScan"
    public static let reminderScanKey = "ReminderScanAt"
    /// The build (`CFBundleVersion`) of the last launch, written at every
    /// launch (story "Scan after an update when the saved report cannot be read").
    public static let lastLaunchedBuildKey = "LastLaunchedBuild"
    /// The scheduled scan made due at the first launch of a new build whose
    /// saved report is of an older schema (menu bar ADR §5, amended 2026-11-05).
    public static let updateScanDueKey = "UpdateScanDue"
    /// The bundle path and build the scheduler agent was last registered
    /// from (story "Scheduled scans: register the agent again after an update").
    public static let schedulerRegistrationKey = "SchedulerRegistration"
    /// When the scheduler agent's last tick ran, by the clock and by the
    /// uptime (`LastAgentTickAt`).
    public static let lastAgentTickKey = "LastAgentTickAt"
    /// Since when the agent's ticks are watched: written once with no tick
    /// yet, and again at a re-registration and at Fix.
    public static let agentWatchedSinceKey = "AgentWatchedSince"
    public static let pendingNotificationKey = "PendingNotification"
    public static let permissionPendingKey = "NotificationPermissionPending"
    public static let permissionAskedKey = "NotificationPermissionAsked"
    public static let oldScanOfferDismissedKey = "OldScanOfferDismissed"
    public static let foundUpdateKey = "FoundUpdate"
    public static let reviewSortKey = "ReviewSort"
    public static let growthBadgePendingKey = "GrowthBadgePending"
    public static let growthBadgedForKey = "GrowthBadgedFor"

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

    /// Turning scheduled scans off also drops the reminder scan and a
    /// notification waiting for working hours (B9).
    public func setSchedule(_ schedule: Schedule) {
        defaults.set(schedule.enabled, forKey: Self.scheduleEnabledKey)
        defaults.set(schedule.repeats.rawValue, forKey: Self.scheduleRepeatKey)
        defaults.set(schedule.weekday, forKey: Self.scheduleWeekdayKey)
        defaults.set(schedule.hour, forKey: Self.scheduleHourKey)
        if !schedule.enabled {
            setReminderScan(nil)
            setPendingNotification(nil)
            setUpdateScanDue(false)
        }
    }

    // MARK: Scheduled scans ("Background scan and notifications in the app")

    /// When the last scheduled scan ran: the due function's record, never
    /// `last-scan.json`, which a Scan now or a terminal `ddc scan` also writes.
    public var lastScheduledScan: Date? { defaults.object(forKey: Self.lastScheduledScanKey) as? Date }

    public func setLastScheduledScan(_ date: Date) {
        defaults.set(date, forKey: Self.lastScheduledScanKey)
    }

    /// The one extra scan Remind me tomorrow adds to a weekly schedule.
    public var reminderScan: Date? { defaults.object(forKey: Self.reminderScanKey) as? Date }

    public func setReminderScan(_ date: Date?) {
        if let date { defaults.set(date, forKey: Self.reminderScanKey) } else { defaults.removeObject(forKey: Self.reminderScanKey) }
    }

    /// `LastLaunchedBuild`; nil before the first launch of a build that writes it.
    public var lastLaunchedBuild: String? { defaults.string(forKey: Self.lastLaunchedBuildKey) }

    public func setLastLaunchedBuild(_ build: String) {
        defaults.set(build, forKey: Self.lastLaunchedBuildKey)
    }

    /// `SchedulerRegistration`: where the agent was registered from; nil
    /// before the first launch that records it.
    public var registeredBundle: RegisteredBundle? {
        defaults.data(forKey: Self.schedulerRegistrationKey).flatMap { try? JSONDecoder().decode(RegisteredBundle.self, from: $0) }
    }

    public func setRegisteredBundle(_ bundle: RegisteredBundle) {
        if let data = try? JSONEncoder().encode(bundle) { defaults.set(data, forKey: Self.schedulerRegistrationKey) }
    }

    public var lastAgentTick: AgentTickMark? { mark(Self.lastAgentTickKey) }

    public func setLastAgentTick(_ mark: AgentTickMark) { setMark(mark, Self.lastAgentTickKey) }

    public var agentWatchedSince: AgentTickMark? { mark(Self.agentWatchedSinceKey) }

    public func setAgentWatchedSince(_ mark: AgentTickMark) { setMark(mark, Self.agentWatchedSinceKey) }

    private func mark(_ key: String) -> AgentTickMark? {
        defaults.data(forKey: key).flatMap { try? JSONDecoder().decode(AgentTickMark.self, from: $0) }
    }

    private func setMark(_ mark: AgentTickMark, _ key: String) {
        if let data = try? JSONEncoder().encode(mark) { defaults.set(data, forKey: key) }
    }

    /// `UpdateScanDue`: a scan after the update is due until a readable
    /// report loads or scheduled scans are turned off.
    public var updateScanDue: Bool { defaults.bool(forKey: Self.updateScanDueKey) }

    public func setUpdateScanDue(_ due: Bool) {
        if due { defaults.set(true, forKey: Self.updateScanDueKey) } else { defaults.removeObject(forKey: Self.updateScanDueKey) }
    }

    /// A decision held back by working hours; a newer one replaces it.
    public var pendingNotification: PendingNotification? {
        defaults.data(forKey: Self.pendingNotificationKey).flatMap { try? JSONDecoder().decode(PendingNotification.self, from: $0) }
    }

    public func setPendingNotification(_ pending: PendingNotification?) {
        if let pending, let data = try? JSONEncoder().encode(pending) {
            defaults.set(data, forKey: Self.pendingNotificationKey)
        } else {
            defaults.removeObject(forKey: Self.pendingNotificationKey)
        }
    }

    /// A `--scheduled` instance found the notification permission not asked
    /// yet (B4): the next popover open offers Allow Notifications.
    public var notificationPermissionPending: Bool { defaults.bool(forKey: Self.permissionPendingKey) }

    public func setNotificationPermissionPending(_ on: Bool) {
        defaults.set(on, forKey: Self.permissionPendingKey)
    }

    /// The app asked for notification permission once already.
    public var notificationPermissionAsked: Bool { defaults.bool(forKey: Self.permissionAskedKey) }

    public func setNotificationPermissionAsked() {
        defaults.set(true, forKey: Self.permissionAskedKey)
    }

    /// The popover's offer to replace the 0.1.0 agent was declined; Settings keeps it.
    public var oldScanOfferDismissed: Bool { defaults.bool(forKey: Self.oldScanOfferDismissedKey) }

    public func setOldScanOfferDismissed() {
        defaults.set(true, forKey: Self.oldScanOfferDismissedKey)
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

    // MARK: Updates ("Sparkle updates: check daily, Update and Relaunch, and the cask's auto_updates")

    /// The last update a check found, so the line survives a restart; `UpdateModel` reads it.
    public var foundUpdate: UpdateRecord? {
        defaults.data(forKey: Self.foundUpdateKey).flatMap { try? JSONDecoder().decode(UpdateRecord.self, from: $0) }
    }

    public func setFoundUpdate(_ record: UpdateRecord?) {
        if let record, let data = try? JSONEncoder().encode(record) {
            defaults.set(data, forKey: Self.foundUpdateKey)
        } else {
            defaults.removeObject(forKey: Self.foundUpdateKey)
        }
    }

    // MARK: The Review window ("Review window: sort a group's rows by name, size or last change")

    /// The rows' order, kept across groups and launches; a missing or
    /// unreadable value reads as the default, Size largest first.
    public var reviewSort: ReviewSort {
        defaults.data(forKey: Self.reviewSortKey).flatMap { try? JSONDecoder().decode(ReviewSort.self, from: $0) } ?? .default
    }

    public func setReviewSort(_ sort: ReviewSort) {
        if let data = try? JSONEncoder().encode(sort) { defaults.set(data, forKey: Self.reviewSortKey) }
    }

    /// A notification was posted and the Review window has not opened since.
    public var badgePending: Bool { defaults.bool(forKey: Self.badgePendingKey) }

    public func notificationPosted() {
        defaults.set(true, forKey: Self.badgePendingKey)
    }

    public func clearBadge() {
        defaults.removeObject(forKey: Self.badgePendingKey)
    }

    // MARK: The growth badge ("Popover: say when used space grew since the last scan")

    /// `ddc space` said `grew` or `low-disk` for a baseline, and the popover
    /// has not opened since. Its own flag beside the notification's, under the
    /// same "Show a badge" preference (menu bar ADR §5, "A second source").
    public var growthBadgePending: Bool { defaults.bool(forKey: Self.growthBadgePendingKey) }

    /// The baseline the growth badge was last set for: once per baseline.
    public var growthBadgedFor: GrowthBadge.Key? {
        defaults.data(forKey: Self.growthBadgedForKey).flatMap { try? JSONDecoder().decode(GrowthBadge.Key.self, from: $0) }
    }

    /// Sets the flag and records the baseline it is for.
    public func setGrowthBadge(for key: GrowthBadge.Key) {
        if let data = try? JSONEncoder().encode(key) { defaults.set(data, forKey: Self.growthBadgedForKey) }
        defaults.set(true, forKey: Self.growthBadgePendingKey)
    }

    /// The flag only: the record stays, so the same baseline never sets it again.
    public func clearGrowthBadge() {
        defaults.removeObject(forKey: Self.growthBadgePendingKey)
    }
}

/// The bundle the scheduler agent was registered from: launchd keeps the
/// registration of the build that made it (story "Scheduled scans: register
/// the agent again after an update"). Not `LastLaunchedBuild`, which every
/// launch writes: this one only while scheduled scans are on.
public struct RegisteredBundle: Codable, Sendable, Equatable {
    public let path: String
    public let build: String

    public init(path: String, build: String) {
        self.path = path
        self.build = build
    }
}

/// A moment by the wall clock and by `ProcessInfo.systemUptime`, which does
/// not advance while the Mac sleeps (story "Scheduled scans: register the
/// agent again after an update").
public struct AgentTickMark: Codable, Sendable, Equatable {
    public let at: Date
    public let uptime: TimeInterval
    /// Written by Fix: the line after it offers Login Items instead.
    public let byFix: Bool

    public init(at: Date, uptime: TimeInterval, byFix: Bool = false) {
        self.at = at
        self.uptime = uptime
        self.byFix = byFix
    }
}
