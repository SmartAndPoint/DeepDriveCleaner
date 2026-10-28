import Foundation

/// The Schedule pane's choice (artboard 10), kept in `UserDefaults`: the
/// scheduler and the due function of "Background scan and notifications in
/// the app" read it. On by default, daily at 10:00 (decision D1).
public struct Schedule: Sendable, Equatable {
    public enum Repeat: String, CaseIterable, Sendable {
        case daily, weekly

        public var title: String {
            switch self {
            case .daily: "Daily"
            case .weekly: "Weekly"
            }
        }
    }

    /// "Scan automatically".
    public var enabled = true
    public var repeats = Repeat.daily
    /// `Calendar` numbering, Sunday = 1; used by Weekly only.
    public private(set) var weekday = 2
    /// A whole hour, 0 to 23: the Mac checks once an hour.
    public private(set) var hour = 10

    public init() {}

    public init(enabled: Bool, repeats: Repeat, weekday: Int, hour: Int) {
        self.enabled = enabled
        self.repeats = repeats
        setWeekday(weekday)
        setHour(hour)
    }

    /// Out of range is clamped: only whole hours of one day exist.
    public mutating func setHour(_ hour: Int) {
        self.hour = min(max(hour, 0), 23)
    }

    public mutating func setWeekday(_ weekday: Int) {
        self.weekday = (1...7).contains(weekday) ? weekday : 2
    }

    /// Every hour the picker offers.
    public static let hours = Array(0...23)
    /// Monday first, as the pane lists them.
    public static let weekdays = [2, 3, 4, 5, 6, 7, 1]

    /// "10:00".
    public static func hourText(_ hour: Int) -> String {
        String(format: "%02d:00", hour)
    }

    /// "Monday".
    public static func weekdayName(_ weekday: Int, calendar: Calendar = Calendar(identifier: .gregorian)) -> String {
        var calendar = calendar
        calendar.locale = Locale(identifier: "en_US")
        return calendar.weekdaySymbols[(weekday - 1) % 7]
    }

    /// The Next scan line: "Off…" and "Paused…" first, else what the
    /// popover footer's seam gives. Nil hides the line: scheduling is on and
    /// the seam says off (no scheduler).
    public func nextScanLine(
        paused: Bool,
        next scan: NextScan,
        now: Date,
        calendar: Calendar = .current,
        locale: Locale = .autoupdatingCurrent
    ) -> String? {
        if !enabled { return "Off: scan from the menu or with ddc scan" }
        if paused { return "Paused: the schedule is kept for Resume" }
        let next: Date
        switch scan {
        case .off: return nil
        case .replaceOldScanFirst: return Wording.replaceOldScanFirst
        case .at(let date): next = date
        }
        // A missed slot, or a Resume: the next hourly check runs it.
        if next <= now { return "Within the hour" }
        let time = next.formatted(Date.FormatStyle(date: .omitted, time: .shortened, locale: locale, calendar: calendar, timeZone: calendar.timeZone))
        if calendar.isDate(next, inSameDayAs: now) { return "Today at \(time)" }
        if let tomorrow = calendar.date(byAdding: .day, value: 1, to: now), calendar.isDate(next, inSameDayAs: tomorrow) {
            return "Tomorrow at \(time)"
        }
        return "\(calendar.weekdaySymbols[calendar.component(.weekday, from: next) - 1]) at \(time)"
    }
}

/// "After a scheduled scan" in the Automation pane. The third choice,
/// cleaning download caches automatically, is drawn but cannot be picked:
/// it belongs to "Automatic cleaning of download caches after a scheduled
/// scan" (decision D3), so it is never stored.
public enum AfterScheduledScan: String, CaseIterable, Sendable {
    /// Nothing is removed until the user reviews it. The default.
    case notify
    /// No notification; the badge and the popover only.
    case menuBarOnly

    public var title: String {
        switch self {
        case .notify: "Notify me"
        case .menuBarOnly: "Only show it in the menu bar"
        }
    }
}

/// The scheduler behind "Scan automatically", filled by "Background scan
/// and notifications in the app". Until then the choice is only stored.
@MainActor
public protocol SchedulerControl: AnyObject {
    /// The Schedule pane changed the choice: register, move or unregister.
    func scheduleChanged(_ schedule: Schedule)
    /// The Permissions pane's "Scheduled scan" row; nil hides it.
    var registration: SchedulerRegistration? { get }
    /// "Old background scan found": a `ddc schedule install` agent still runs.
    var legacyAgentFound: Bool { get }
    /// Replace It.
    func replaceLegacyAgent() async
    /// Why the last Replace It changed nothing; nil when it did not fail.
    var replaceFailure: String? { get }
}

/// What the "Scheduled scan" row says.
public enum SchedulerRegistration: Sendable, Equatable {
    case allowed
    /// macOS waits for the user in Login Items & Extensions.
    case requiresApproval
    case off
}

/// No scheduler yet: nothing registers, and the rows stay hidden.
@MainActor
public final class NoSchedulerControl: SchedulerControl {
    public init() {}
    public func scheduleChanged(_ schedule: Schedule) {}
    public var registration: SchedulerRegistration? { nil }
    public var legacyAgentFound: Bool { false }
    public func replaceLegacyAgent() async {}
    public var replaceFailure: String? { nil }
}
