import Foundation

/// One hourly tick of the scheduler (menu bar ADR §5; story "Background scan
/// and notifications in the app"): a scheduled scan when the due function
/// says so, then the notification it decided, or the one still waiting for
/// working hours. Also the notification's answers, Review and Remind Me
/// Tomorrow, and the one question for notification permission.
///
/// Only scheduled scans notify. A tick never activates the app and never
/// asks for permission: a `--scheduled` instance runs it and quits.
@MainActor
public final class ScheduledWork {
    public enum TickResult: Sendable, Equatable {
        case notDue
        case scanned
        /// A clean held the lock: still due at the next tick.
        case busy
        case failed
    }

    private let popover: PopoverModel
    private let preferences: AppPreferences
    private let runner: (any CLIRunning)?
    private let notifications: any NotificationPosting
    private let legacyAgentFound: @MainActor () -> Bool
    private let clock: @MainActor () -> Date
    private let calendar: Calendar
    private let log: @MainActor (String) -> Void
    /// Opens System Settings → Notifications: macOS asks only once, so a
    /// denied permission can be changed only there.
    private let openNotificationSettings: @MainActor () -> Void
    /// The tick in flight: a reopen during a scheduled scan joins it, so
    /// it never scans twice nor posts twice.
    private var running: Task<TickResult, Never>?

    public init(
        popover: PopoverModel,
        preferences: AppPreferences,
        runner: (any CLIRunning)?,
        notifications: any NotificationPosting,
        legacyAgentFound: @escaping @MainActor () -> Bool,
        clock: @escaping @MainActor () -> Date = { .now },
        calendar: Calendar = .current,
        openNotificationSettings: @escaping @MainActor () -> Void = {},
        log: @escaping @MainActor (String) -> Void = { _ in }
    ) {
        self.openNotificationSettings = openNotificationSettings
        self.popover = popover
        self.preferences = preferences
        self.runner = runner
        self.notifications = notifications
        self.legacyAgentFound = legacyAgentFound
        self.clock = clock
        self.calendar = calendar
        self.log = log
    }

    // MARK: - The due function

    public var inputs: ScanDue.Inputs {
        ScanDue.Inputs(
            schedule: preferences.schedule,
            lastScheduled: preferences.lastScheduledScan,
            reminder: preferences.reminderScan,
            paused: preferences.scansPaused,
            legacyAgentFound: legacyAgentFound()
        )
    }

    public func due() -> ScanDue.Due? {
        ScanDue.due(inputs, now: clock(), calendar: calendar)
    }

    /// The popover footer's and the Schedule pane's seam.
    public func next() -> NextScan {
        ScanDue.next(inputs, now: clock(), calendar: calendar)
    }

    // MARK: - A tick

    /// A scan if one is due, then the pending notification. Returns after
    /// the post and its `notify ack`, so a `--scheduled` instance quits only
    /// after both. A tick while one runs gets that one's result: the due
    /// function cannot see a scheduled scan until it has ended.
    @discardableResult
    public func tick() async -> TickResult {
        if let running { return await running.value }
        let task = Task { await runTick() }
        running = task
        let result = await task.value
        running = nil
        return result
    }

    private func runTick() async -> TickResult {
        let now = clock()
        var result = TickResult.notDue
        if let due = due() {
            log("tick: \(due) scan due")
            await popover.start()
            switch await popover.scheduledScan() {
            case .busy:
                log("tick: a clean holds the lock; the scan stays due")
                result = .busy
            case .failed(let message):
                log("tick: scheduled scan failed: \(message)")
                result = .failed
            case .scanned(let decision):
                result = .scanned
                preferences.setLastScheduledScan(now)
                // Only a reminder that was due is used up; a catch-up slot before it keeps it.
                if let reminder = preferences.reminderScan, reminder <= now { preferences.setReminderScan(nil) }
                // A newer decision replaces a pending one, a quiet one too.
                preferences.setPendingNotification(decision.notify ? pending(for: decision, at: now) : nil)
            }
        } else {
            log("tick: nothing due")
        }
        await flushPending()
        return result
    }

    private func pending(for decision: NotifyDecision, at now: Date) -> PendingNotification {
        // The summary's groups under the current settings: the popover's biggest finds.
        let summary = popover.loadedReport.map { ReportSummary.make(report: $0, settings: popover.settings, rules: popover.rules, top: 3) }
        let text = NotificationText.make(decision, groups: summary?.topGroups.map(\.name) ?? [])
        return PendingNotification(text: text, bytes: decision.reclaimable, decided: now)
    }

    /// Posts the waiting notification: also while paused, since it reports
    /// a scan already made.
    private func flushPending() async {
        guard let pending = preferences.pendingNotification else { return }
        if preferences.afterScheduledScan == .menuBarOnly {
            // B3: no notification and no ack; the badge, if it is on, says it.
            preferences.setPendingNotification(nil)
            popover.notificationPosted()
            log("notification: menu bar only")
            return
        }
        if preferences.workingHoursOnly, !ScanDue.isWorkingHours(clock(), calendar: calendar) {
            log("notification: kept for working hours")
            return
        }
        switch await notifications.authorization() {
        case .authorized:
            preferences.setPendingNotification(nil)
            guard await notifications.post(pending.text) else {
                log("notification: macOS did not accept it; not acknowledged")
                return
            }
            popover.notificationPosted()
            await acknowledge(pending.bytes)
        case .denied:
            // B8: dropped and not acknowledged, so the next scan decides again; the icon still says it.
            preferences.setPendingNotification(nil)
            popover.notificationPosted()
            log("notification: not allowed; dropped")
        case .notDetermined:
            // B4: a tick never asks; the next popover open offers it.
            preferences.setPendingNotification(nil)
            preferences.setNotificationPermissionPending(true)
            log("notification: permission not asked yet; dropped, flag set")
        }
    }

    private func acknowledge(_ bytes: UInt64) async {
        guard let runner else { return }
        let result = await runner.run(.notify, ["notify", "ack", "--bytes", String(bytes)])
        log("notify ack \(bytes): \(result.outcome)")
    }

    // MARK: - The notification's answers

    /// Review, or a click on the notification: the report on disk first,
    /// then the Review window.
    public func review() async {
        await popover.start()
        _ = await popover.reloadReport()
        popover.review()
    }

    /// Remind Me Tomorrow (B7): no notification from scans that start before
    /// the schedule's hour on the day after the tap. With a weekly schedule
    /// one reminder scan runs then, so "tomorrow" stays true.
    public func remindTomorrow() async {
        let schedule = preferences.schedule
        let until = ScanDue.remindAt(schedule, tappedAt: clock(), calendar: calendar)
        popover.clearBadge()
        if schedule.repeats == .weekly { preferences.setReminderScan(until) }
        guard let runner else { return }
        let result = await runner.run(.notify, ["notify", "snooze", "--until", Self.utc(until)])
        log("notify snooze until \(Self.utc(until)): \(result.outcome)")
    }

    /// RFC 3339 in UTC with `Z`, the only form `notify snooze --until` takes.
    static func utc(_ date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.timeZone = TimeZone(identifier: "UTC")
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.string(from: date)
    }

    // MARK: - Permission

    /// A normal launch after the first scan asks once; never a tick.
    public func askPermissionOnce() async {
        guard !preferences.notificationPermissionAsked, popover.loadedReport != nil else { return }
        preferences.setNotificationPermissionAsked()
        if await notifications.authorization() == .notDetermined {
            let granted = await notifications.requestAuthorization()
            log("notification permission asked at launch: \(granted)")
        }
        preferences.setNotificationPermissionPending(false)
    }

    /// Allow Notifications in the popover, and the end of the migration.
    /// Once denied, macOS shows no question again: System Settings opens instead.
    public func allowNotifications() async {
        preferences.setNotificationPermissionAsked()
        preferences.setNotificationPermissionPending(false)
        switch await notifications.authorization() {
        case .denied:
            log("notification permission denied: opening System Settings")
            openNotificationSettings()
        case .authorized:
            break
        case .notDetermined:
            let granted = await notifications.requestAuthorization()
            log("notification permission asked: \(granted)")
        }
        await popover.refresh()
    }
}
