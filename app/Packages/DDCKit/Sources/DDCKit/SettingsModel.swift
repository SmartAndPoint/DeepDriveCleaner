import Foundation
import Observation

/// The Settings window's state (story "Settings: folders, types,
/// notifications"; artboard 10): one model behind the six panes, so the
/// panes only draw.
///
/// What the CLI also reads goes to `settings.json` through `SettingsStore`;
/// what only the app uses goes to `AppPreferences`. The popover's own state
/// (the icon switches, Pause, Full Disk Access, the rules) is the popover
/// model's, so both always agree. Settings never scans and never cleans: a
/// change applies to the loaded report at once, and to the next scan.
@MainActor
@Observable
public final class SettingsModel {
    /// `DEFAULT_MIN_AGE` of the CLI, in days.
    nonisolated public static let defaultMinAgeDays: UInt64 = 14
    /// The one notice of the Automation pane while scheduled scans are off.
    nonisolated public static let automationOff = "Scheduled scans are off in Schedule, so nothing here runs on its own."

    public let store: SettingsStore
    /// The home folder: the default root, and `~` in every path shown.
    public let home: String
    /// The General pane's "Command-line tool" row.
    public let commandLineTool: String
    /// Why the last folder change was refused; cleared by the next change.
    public private(set) var folderMessage: String?
    public private(set) var schedule: Schedule
    public private(set) var afterScheduledScan: AfterScheduledScan
    public private(set) var workingHoursOnly: Bool
    public private(set) var deletePermanently: Bool
    public private(set) var registration: SchedulerRegistration?
    public private(set) var legacyAgentFound = false

    @ObservationIgnored private let popover: PopoverModel
    @ObservationIgnored private let preferences: AppPreferences
    @ObservationIgnored private let scheduler: any SchedulerControl
    @ObservationIgnored private let clock: @MainActor () -> Date

    public init(
        store: SettingsStore,
        popover: PopoverModel,
        preferences: AppPreferences,
        scheduler: any SchedulerControl = NoSchedulerControl(),
        home: String = FileManager.default.homeDirectoryForCurrentUser.path,
        commandLineTool: String = "",
        clock: @escaping @MainActor () -> Date = { .now }
    ) {
        self.store = store
        self.popover = popover
        self.preferences = preferences
        self.scheduler = scheduler
        self.home = home.count > 1 && home.hasSuffix("/") ? String(home.dropLast()) : home
        self.commandLineTool = commandLineTool
        self.clock = clock
        schedule = preferences.schedule
        afterScheduledScan = preferences.afterScheduledScan
        workingHoursOnly = preferences.workingHoursOnly
        deletePermanently = preferences.deleteReviewPermanently
        registration = scheduler.registration
        legacyAgentFound = scheduler.legacyAgentFound
    }

    /// When the window opens and when the app becomes active: the file, the
    /// preferences and the scheduler may have changed elsewhere.
    public func refresh() {
        store.reload()
        schedule = preferences.schedule
        afterScheduledScan = preferences.afterScheduledScan
        workingHoursOnly = preferences.workingHoursOnly
        deletePermanently = preferences.deleteReviewPermanently
        registration = scheduler.registration
        legacyAgentFound = scheduler.legacyAgentFound
    }

    /// Why `settings.json` cannot be changed here; every pane that writes it says so.
    public var problem: String? { store.problem }

    // MARK: - General

    public var showSize: Bool { popover.showSize }
    public func setShowSize(_ on: Bool) { popover.setShowSize(on) }

    public var showBadge: Bool { popover.showBadge }
    public func setShowBadge(_ on: Bool) { popover.setShowBadge(on) }

    public func setDeletePermanently(_ on: Bool) {
        preferences.setDeleteReviewPermanently(on)
        deletePermanently = on
    }

    // MARK: - Folders

    public var folders: FolderList { FolderList(file: store.file, home: home) }

    public func addRoot(_ path: String) {
        edit { file throws(FolderList.Refusal) in try FolderList.addRoot(path, home: home, to: &file) }
    }

    public func removeRoot(_ path: String) {
        edit { file throws(FolderList.Refusal) in try FolderList.removeRoot(path, home: home, from: &file) }
    }

    public func addExclusion(_ path: String, mode: FolderList.Mode) {
        edit { file throws(FolderList.Refusal) in try FolderList.addExclusion(path, mode: mode, home: home, to: &file) }
    }

    public func removeExclusion(_ path: String) {
        edit { file in FolderList.removeExclusion(path, from: &file) }
    }

    public func setMode(_ mode: FolderList.Mode, of path: String) {
        edit { file in FolderList.setMode(mode, of: path, in: &file) }
    }

    /// A refusal is checked against the file as it is on disk now, inside
    /// the store's read-modify-write.
    private func edit(_ change: (inout SettingsFile) throws(FolderList.Refusal) -> Void) {
        var refusal: FolderList.Refusal?
        store.update { file in
            do throws(FolderList.Refusal) {
                try change(&file)
            } catch {
                refusal = error
            }
        }
        folderMessage = refusal?.message
    }

    /// The Folders banner and the Permissions row: the popover's live probe.
    public var fullDiskAccess: Bool { popover.fullDiskAccess.isGranted }

    /// "Not readable": one row per prompting area of the app's last Scan now.
    public var notReadable: [(path: String, shown: String)] {
        popover.notReadable.map { ($0, Wording.tilde($0, home: home)) }
    }

    // MARK: - Schedule

    public func setSchedule(_ schedule: Schedule) {
        guard schedule != self.schedule else { return }
        preferences.setSchedule(schedule)
        self.schedule = schedule
        scheduler.scheduleChanged(schedule)
    }

    /// Pause: the same `ScansPaused` as the menu's, through the popover.
    public var paused: Bool { popover.paused }
    public func setPaused(_ on: Bool) { popover.setPaused(on) }

    /// "Off…", "Paused…", or the date the popover footer's seam gives; nil
    /// hides the line.
    public var nextScan: String? {
        schedule.nextScanLine(paused: popover.paused, next: popover.nextScanDate, now: clock())
    }

    // MARK: - Automation

    /// Nil while scheduled scans are on; otherwise the pane says so and is disabled.
    public var automationNote: String? { schedule.enabled ? nil : Self.automationOff }

    public func setAfterScheduledScan(_ choice: AfterScheduledScan) {
        preferences.setAfterScheduledScan(choice)
        afterScheduledScan = choice
    }

    public func setWorkingHoursOnly(_ on: Bool) {
        preferences.setWorkingHoursOnly(on)
        workingHoursOnly = on
    }

    /// "At least this much can be freed", in whole GiB.
    public var thresholdGiB: Int { Int((store.file.notifyThreshold ?? SharedSettings.defaultThreshold) >> 30) }

    public func setThresholdGiB(_ gib: Int) {
        store.update { $0.notifyThreshold = UInt64(max(gib, 1)) << 30 }
    }

    /// "The disk is almost full"; on when the file says nothing.
    public var lowDisk: Bool { store.file.notifyLowDisk ?? true }

    public func setLowDisk(_ on: Bool) {
        store.update { $0.notifyLowDisk = on }
    }

    // MARK: - Types

    /// Empty until the popover has read `rules --json`.
    public var types: [TypesRow] {
        popover.rules.map { TypesRows.make(rules: $0, disabled: store.file.disabledRules) } ?? []
    }

    public func setType(_ row: TypesRow, on: Bool) {
        store.update { TypesRows.set(row, on: on, in: &$0) }
    }

    /// "Projects count as idle after N days": from the next scan.
    public var minAgeDays: Int { Int(store.file.minAgeDays ?? Self.defaultMinAgeDays) }

    public func setMinAgeDays(_ days: Int) {
        store.update { $0.minAgeDays = UInt64(min(max(days, 0), Int(SharedSettings.maxMinAgeDays))) }
    }

    /// "Hide items smaller than N MB", as the CLI's `M`: 2^20 bytes.
    public var minSizeMB: Int { Int((store.file.minSize ?? 0) >> 20) }

    public func setMinSizeMB(_ mb: Int) {
        store.update { $0.minSize = mb > 0 ? UInt64(mb) << 20 : nil }
    }

    // MARK: - Permissions

    public func replaceLegacyAgent() {
        scheduler.replaceLegacyAgent()
        legacyAgentFound = scheduler.legacyAgentFound
    }
}
