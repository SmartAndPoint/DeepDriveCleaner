import Foundation

/// The next scheduled scan, as the popover footer and the Schedule pane say
/// it: the seam "Settings: folders, types, notifications" left.
public enum NextScan: Sendable, Equatable {
    /// "Scan automatically" is off.
    case off
    /// The 0.1.0 agent still runs: the app schedules nothing until it is replaced (B1).
    case replaceOldScanFirst
    /// At or before now: due at the next hourly tick.
    case at(Date)
}

/// Where the popover's Next scan line leads (story "Popover: the Next scan
/// line opens the Schedule settings"): the Settings pane that can change
/// what it says. Its tooltip and hint live here, so they follow the route.
public enum SettingsDestination: Sendable, Equatable {
    case schedule
    /// Where Replace It is, for the 0.1.0 agent.
    case permissions

    public var help: String {
        switch self {
        case .schedule: "Change the schedule in Settings"
        case .permissions: "Replace the old background scan in Settings"
        }
    }

    /// The accessibility hint.
    public var hint: String {
        switch self {
        case .schedule: "Opens Settings, Schedule"
        case .permissions: "Opens Settings, Permissions"
        }
    }
}

extension NextScan {
    public var settingsDestination: SettingsDestination {
        switch self {
        case .off, .at: .schedule
        case .replaceOldScanFirst: .permissions
        }
    }
}

/// The due function of menu bar ADR §5: whether a scheduled scan is due at
/// this tick, and when the next one is. Pure: the schedule, the last
/// scheduled scan, the reminder and the clock come in.
///
/// The last scheduled scan is the app's own record, never `last-scan.json`:
/// a Scan now or a terminal `ddc scan` neither moves the slot nor suppresses
/// that day's notification. A slot missed while the Mac slept is due at the
/// next tick, because the last scheduled scan is older than it.
public enum ScanDue {
    public enum Due: Sendable, Equatable {
        /// The schedule's slot (daily or weekly, a whole hour), or the first tick of a new install.
        case slot
        /// The one extra scan Remind me tomorrow adds to a weekly schedule.
        case reminder
    }

    public struct Inputs: Sendable, Equatable {
        public var schedule: Schedule
        /// When the last scheduled scan ran; nil on a new install.
        public var lastScheduled: Date?
        /// `ReminderScanAt`.
        public var reminder: Date?
        public var paused: Bool
        /// The 0.1.0 agent's plist exists.
        public var legacyAgentFound: Bool

        public init(schedule: Schedule, lastScheduled: Date?, reminder: Date?, paused: Bool, legacyAgentFound: Bool) {
            self.schedule = schedule
            self.lastScheduled = lastScheduled
            self.reminder = reminder
            self.paused = paused
            self.legacyAgentFound = legacyAgentFound
        }
    }

    /// Nil when nothing is due: off, paused, the legacy agent found (B1), or
    /// the last slot and any reminder already scanned.
    public static func due(_ inputs: Inputs, now: Date, calendar: Calendar = .current) -> Due? {
        guard inputs.schedule.enabled, !inputs.paused, !inputs.legacyAgentFound else { return nil }
        return pending(inputs, now: now, calendar: calendar)?.due
    }

    /// The footer's and the Schedule pane's next scan: a date at or before
    /// `now` while one is due (also while paused, so a Resume reads "within
    /// the hour", never a slot in the past), else the earlier of the next
    /// slot and the reminder.
    public static func next(_ inputs: Inputs, now: Date, calendar: Calendar = .current) -> NextScan {
        guard inputs.schedule.enabled else { return .off }
        guard !inputs.legacyAgentFound else { return .replaceOldScanFirst }
        if let pending = pending(inputs, now: now, calendar: calendar) { return .at(pending.since) }
        let slot = nextSlot(inputs.schedule, after: now, calendar: calendar)
        if let reminder = inputs.reminder, reminder > now { return .at(min(slot, reminder)) }
        return .at(slot)
    }

    /// What is due with the schedule on, and since when.
    private static func pending(_ inputs: Inputs, now: Date, calendar: Calendar) -> (due: Due, since: Date)? {
        guard let last = inputs.lastScheduled else { return (.slot, now) }
        let slot = lastSlot(inputs.schedule, atOrBefore: now, calendar: calendar)
        if last < slot { return (.slot, slot) }
        if let reminder = inputs.reminder, reminder <= now, last < reminder { return (.reminder, reminder) }
        return nil
    }

    /// The schedule's latest slot at or before `now`.
    static func lastSlot(_ schedule: Schedule, atOrBefore now: Date, calendar: Calendar) -> Date {
        // `nextDate` backwards is strictly before its start: a second later includes `now`.
        calendar.nextDate(after: now.addingTimeInterval(1), matching: components(schedule), matchingPolicy: .nextTime, direction: .backward) ?? now
    }

    /// The schedule's first slot after `now`.
    static func nextSlot(_ schedule: Schedule, after now: Date, calendar: Calendar) -> Date {
        calendar.nextDate(after: now, matching: components(schedule), matchingPolicy: .nextTime) ?? now
    }

    private static func components(_ schedule: Schedule) -> DateComponents {
        var components = DateComponents(hour: schedule.hour, minute: 0, second: 0)
        if schedule.repeats == .weekly { components.weekday = schedule.weekday }
        return components
    }

    /// Remind me tomorrow (B7): the schedule's hour on the calendar day after
    /// the tap, whenever the notification was posted.
    public static func remindAt(_ schedule: Schedule, tappedAt tap: Date, calendar: Calendar = .current) -> Date {
        let tomorrow = calendar.date(byAdding: .day, value: 1, to: calendar.startOfDay(for: tap)) ?? tap
        return calendar.date(bySettingHour: schedule.hour, minute: 0, second: 0, of: tomorrow) ?? tomorrow
    }

    /// "Only during working hours": Monday to Friday, from 9:00 to before 19:00.
    public static func isWorkingHours(_ date: Date, calendar: Calendar = .current) -> Bool {
        let weekday = calendar.component(.weekday, from: date)
        let hour = calendar.component(.hour, from: date)
        return (2...6).contains(weekday) && (9..<19).contains(hour)
    }
}
