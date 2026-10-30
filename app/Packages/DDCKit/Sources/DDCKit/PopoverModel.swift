import Foundation
import Observation

/// The menu bar popover's state (story "Menu bar popover"; artboards 1 and 3
/// of the canvas): the last report as the summary shows it, Scan now, Full
/// Disk Access from the live probe, the notices, and the status item's icon.
///
/// The popover never cleans (ADR §7): Review…, a find and Clean selected…
/// only hand a request to the review seam.
@MainActor
@Observable
public final class PopoverModel {
    public enum ReportState: Equatable {
        case loading
        case none
        case unreadable(String)
        case loaded(ScanReport)
    }

    /// The main block of the popover, in the order the states win.
    public enum Content: Equatable {
        case loading
        case scanning
        case firstScan
        case unreadable(String)
        /// Nothing the default selection would free.
        case nothingFound
        /// Something, but under the notification threshold.
        case nothingWorthCleaning
        case found
    }

    public struct Notice: Identifiable, Equatable, Sendable {
        /// What a notice's button does ("Background scan and notifications in the app").
        public enum Action: Sendable, Equatable {
            /// A scheduled run found the permission not asked yet (B4).
            case allowNotifications
            /// The 0.1.0 agent still runs; dismissing the notice declines.
            case replaceOldScan

            public var title: String {
                switch self {
                case .allowNotifications: "Allow Notifications"
                case .replaceOldScan: "Replace It"
                }
            }
        }

        public let id: String
        public let text: String
        public var action: Action?

        public init(id: String, text: String, action: Action? = nil) {
            self.id = id
            self.text = text
            self.action = action
        }
    }

    /// The notices' ids the scheduler raises.
    public static let permissionNotice = "permission"
    public static let oldScanNotice = "old-scan"
    public static let schedulerNotice = "scheduler"

    /// The running scan.
    public struct ScanProgress: Equatable, Sendable {
        public let started: Date
        /// Files read so far, from the CLI's `walk` events; nil before the first.
        public var files: UInt64?
    }

    public private(set) var report: ReportState = .loading
    public private(set) var summary: ReportSummary?
    /// Nil when `settings.json` cannot be read: the report's own selection stands.
    public private(set) var settings: SharedSettings?
    public private(set) var rules: RuleList?
    public private(set) var scan: ScanProgress?
    /// A clean of the Review window, or the rescan after it, is in flight.
    public private(set) var cleaning = false
    /// Pause in the menu: scans wait for Resume.
    public private(set) var paused = false
    /// An update's relaunch waits for the runner or runs (`UpdateModel.jobsHeld`):
    /// no scan or clean starts from here.
    public private(set) var updateWaiting = false
    public private(set) var fullDiskAccess: FullDiskAccess = .granted
    public private(set) var notices: [Notice] = []
    public private(set) var now: Date
    public private(set) var badgePending = false
    public private(set) var showSize = true
    public private(set) var showBadge = true
    /// The prompting areas the app's last own scan could not read (`NotReadable`).
    public private(set) var notReadable: [String] = []
    /// The class switches; in memory, so every launch starts from the default selection.
    public var includeSafe = true
    public var includeRebuildable = true

    @ObservationIgnored private let runner: (any CLIRunning)?
    @ObservationIgnored private let data: DataDirectory
    @ObservationIgnored private let probe: FullDiskAccessProbe
    @ObservationIgnored private let preferences: AppPreferences
    @ObservationIgnored private let router: any ReviewRouting
    @ObservationIgnored private let nextScan: @MainActor () -> NextScan
    @ObservationIgnored private let legacyAgentFound: @MainActor () -> Bool
    @ObservationIgnored private let clock: @MainActor () -> Date
    @ObservationIgnored private let log: @MainActor (String) -> Void
    @ObservationIgnored private let appVersion: String?
    @ObservationIgnored private let home: String
    @ObservationIgnored private var reportModified: Date?
    @ObservationIgnored private var settingsModified: Date?
    /// `settings.json` did not read; the warning lasts until it does, whatever its notice.
    private var settingsUnreadable = false
    @ObservationIgnored private var scanTask: Task<Void, Never>?
    @ObservationIgnored private var scheduledTask: Task<ScheduledScan, Never>?
    /// A notice's button: the app runs it (Allow Notifications, Replace It).
    @ObservationIgnored public var onNoticeAction: (@MainActor (Notice.Action) -> Void)?
    @ObservationIgnored private var started = false

    /// `runner` nil: the embedded CLI is missing. `nextScan` is the due
    /// function's next scan ("Background scan and notifications in the app");
    /// `legacyAgentFound` says whether the 0.1.0 agent's plist exists.
    public init(
        runner: (any CLIRunning)?,
        data: DataDirectory,
        probe: FullDiskAccessProbe,
        preferences: AppPreferences,
        router: any ReviewRouting,
        nextScan: @escaping @MainActor () -> NextScan = { .off },
        legacyAgentFound: @escaping @MainActor () -> Bool = { false },
        appVersion: String? = nil,
        clock: @escaping @MainActor () -> Date = { .now },
        log: @escaping @MainActor (String) -> Void = { _ in },
        home: String = FileManager.default.homeDirectoryForCurrentUser.path
    ) {
        self.home = home.count > 1 && home.hasSuffix("/") ? String(home.dropLast()) : home
        self.runner = runner
        self.data = data
        self.probe = probe
        self.preferences = preferences
        self.router = router
        self.nextScan = nextScan
        self.legacyAgentFound = legacyAgentFound
        self.appVersion = appVersion
        self.clock = clock
        self.log = log
        now = clock()
        readPreferences()
    }

    // MARK: - What the popover shows

    /// Not without the CLI, not while paused, and not while an update's install waits.
    public var canScan: Bool { runner != nil && !paused && !updateWaiting }

    public var included: Set<ScanReport.RiskClass> {
        var classes: Set<ScanReport.RiskClass> = []
        if includeSafe { classes.insert(.safe) }
        if includeRebuildable { classes.insert(.rebuildable) }
        return classes
    }

    /// "Selected to clean": the default selection of the classes switched on.
    public var selectedToClean: UInt64 { summary?.selectedBytes(including: included) ?? 0 }

    /// What cleaning that selection frees. When it rounds to another size than
    /// `selectedToClean`, the popover says so: a notification names this amount.
    public var freesAbout: String? {
        guard let summary else { return nil }
        let frees = ByteSize.human(summary.reclaimable(including: included))
        return frees == ByteSize.human(selectedToClean) ? nil : frees
    }

    public var loadedReport: ScanReport? {
        if case .loaded(let report) = report { return report }
        return nil
    }

    public var content: Content {
        if scan != nil { return .scanning }
        switch report {
        case .loading: return .loading
        case .none: return .firstScan
        case .unreadable(let reason): return .unreadable(reason)
        case .loaded:
            guard let summary, summary.reclaimable > 0 else { return .nothingFound }
            return summary.isOverThreshold ? .found : .nothingWorthCleaning
        }
    }

    /// "Last scan 2 h ago · 5.5 M files in 39.6 s".
    public var subtitle: String {
        if cleaning { return "Clean in progress" }
        if scan != nil { return "Scan in progress" }
        switch report {
        case .loading: return ""
        case .none: return "No scan yet"
        case .unreadable: return "The last report could not be read"
        case .loaded(let report):
            var parts = ["Last scan \(Wording.date(rfc3339: report.generatedAt).map { Wording.age(of: $0, now: now) } ?? "at an unknown time")"]
            parts.append("\(Wording.count(report.totals.files)) files in \(Wording.duration(milliseconds: report.durationMs))")
            if report.cancelled { parts.append("incomplete") }
            if let appVersion, report.ddcVersion != appVersion { parts.append("by ddc \(report.ddcVersion)") }
            return parts.joined(separator: " · ")
        }
    }

    /// "in your home folder".
    public var scope: String {
        loadedReport.map { Wording.scope(roots: $0.scanRoots, home: $0.home) } ?? "in your home folder"
    }

    /// Whether the report is of the app's roots; nil without a report or
    /// without readable settings (the CLI refuses those too). Computed when
    /// the report or the settings change, never on a redraw.
    public private(set) var reportScope: ReportScope?

    /// The Whole disk view's Coding agents total in Review: computed with
    /// `reportScope`, when the report, the settings or the rules change.
    public private(set) var codingAgentsInReview: AgentsSummary.InReview?

    /// The popover's line in place of "found in …" for a report of other roots.
    public var scopeNotice: String? {
        guard let reportScope, let report = loadedReport else { return nil }
        return Wording.reportScope(reportScope, roots: report.scanRoots, home: home, reportHome: report.home)
    }

    /// Clean selected…: not for a report of folders outside the app's roots,
    /// and not while an update's install waits.
    public var canCleanSelected: Bool { selectedToClean > 0 && reportScope != .outside && !updateWaiting }

    /// The report's own fact, whatever the probe says now.
    public var withoutAccessNote: String? {
        guard let report = loadedReport, !report.fullDiskAccess else { return nil }
        return "This scan ran without Full Disk Access; part of ~/Library was not read."
    }

    public var nextScanText: String { Wording.nextScan(nextScan(), now: now) }

    /// The due function's next scan, as the footer reads it; Settings' Next scan too.
    public var nextScanValue: NextScan { nextScan() }

    public var threshold: UInt64 { summary?.threshold ?? settings?.threshold ?? SharedSettings.defaultThreshold }

    public var iconState: IconState {
        IconState.make(
            busy: scan != nil,
            cleaning: cleaning,
            paused: paused,
            attention: attention,
            reclaimable: summary?.reclaimable,
            threshold: threshold,
            showSize: showSize,
            badgePending: badgePending,
            showBadge: showBadge
        )
    }

    /// What needs the user: the icon's warning. Each cause lasts as long as
    /// its source: the probe, the CLI, a failed scan's notice (until it is
    /// dismissed or the next scan starts), the report, the settings.
    public var attention: [IconState.Attention] {
        var causes: [IconState.Attention] = []
        if !fullDiskAccess.isGranted { causes.append(.fullDiskAccess) }
        if runner == nil { causes.append(.missingCLI) }
        if notices.contains(where: { $0.id == "scan" }) { causes.append(.scanFailed) }
        if case .unreadable = report { causes.append(.unreadableReport) }
        if settingsUnreadable { causes.append(.unreadableSettings) }
        return causes
    }

    // MARK: - Loading

    /// Probe, settings, the last report, then `rules --json` once, before any
    /// scan the user asks for: every run of the CLI waits for the one before.
    public func start() async {
        guard !started else { return }
        started = true
        probeAccess()
        loadSettings()
        await loadReport()
        if runner == nil {
            setNotice("cli", "The app's embedded CLI is missing, so it cannot scan. Reinstall DeepDriveCleaner.")
        }
        raiseSchedulerNotices()
        await loadRules()
    }

    /// When the popover opens, the app becomes active, and once a minute:
    /// the probe again, and whatever changed on disk (a scheduled scan or a
    /// terminal `ddc scan` writes `last-scan.json` too).
    public func refresh() async {
        now = clock()
        probeAccess()
        readPreferences()
        if DataDirectory.modified(data.settings) != settingsModified { loadSettings() }
        raiseSchedulerNotices()
        // A clean writes the file when it ends; its reload, or the scan after it, shows it.
        if scan == nil, !cleaning, DataDirectory.modified(data.lastScan) != reportModified { await loadReport() }
        // A disk plugged in or out changes the app's roots with no file changed.
        updateScope()
    }

    /// The report a clean edited and saved, read at once, during the clean
    /// too (story "Faster clean: re-check only what the selection needs").
    /// False when it cannot be shown: the caller scans instead.
    public func reloadReport() async -> Bool {
        await loadReport()
        return loadedReport != nil
    }

    /// Allow Notifications while a scheduled run left the flag (B4), and the
    /// offer to replace the 0.1.0 agent while its plist exists and the offer
    /// was not declined (B2).
    private func raiseSchedulerNotices() {
        if preferences.notificationPermissionPending {
            setNotice(Self.permissionNotice, "A scheduled scan had something to tell you, but notifications are not allowed yet.", action: .allowNotifications)
        } else {
            clearNotice(Self.permissionNotice)
        }
        if legacyAgentFound() && !preferences.oldScanOfferDismissed {
            setNotice(Self.oldScanNotice, "The background scan of the command-line tool still runs. The app can scan on its own schedule instead, with notifications you can act on.", action: .replaceOldScan)
        } else {
            clearNotice(Self.oldScanNotice)
        }
    }

    private func probeAccess() {
        fullDiskAccess = probe.check()
    }

    private func readPreferences() {
        paused = preferences.scansPaused
        showSize = preferences.showSize
        showBadge = preferences.showBadge
        badgePending = preferences.badgePending
        notReadable = preferences.notReadable
    }

    private func loadSettings() {
        settingsModified = DataDirectory.modified(data.settings)
        do {
            settings = try SharedSettings.read(at: data.settings)
            settingsUnreadable = false
            clearNotice("settings")
        } catch {
            settings = nil
            settingsUnreadable = true
            switch error {
            case .malformed(_, let reason):
                setNotice("settings", "settings.json could not be read (\(reason)); the scan's own selection is shown.")
            case .otherSchema(let version):
                setNotice("settings", "settings.json is of another version (\(version)); the scan's own selection is shown.")
            }
            log("settings.json unreadable: \(error)")
        }
        recompute()
    }

    private func loadReport() async {
        let url = data.lastScan
        guard let modified = DataDirectory.modified(url) else {
            reportModified = nil
            report = .none
            recompute()
            return
        }
        // Several MB on a full home: decoded off the main actor.
        let decoded = await Task.detached { Result { try ScanReport.read(Data(contentsOf: url)) } }.value
        reportModified = modified
        apply(decoded)
    }

    private func loadRules() async {
        guard rules == nil, let runner else { return }
        let result = await runner.run(.rules, ["rules", "--json"], stderrLine: nil)
        guard result.outcome == .done, let rules = try? RuleList.read(result.stdout) else {
            log("rules --json failed: \(result.outcome); \(result.stderrTail.suffix(3).joined(separator: " | "))")
            return
        }
        self.rules = rules
        recompute()
    }

    private func apply(_ decoded: Result<ScanReport, any Error>) {
        switch decoded {
        case .success(let read):
            report = .loaded(read)
        case .failure(ScanReport.ReadError.otherSchema(let version)):
            report = .unreadable("It was written by another version of ddc (schema \(version)).")
        case .failure(let error):
            report = .unreadable("It is not a scan report this app can read.")
            log("last-scan.json unreadable: \(error)")
        }
        recompute()
    }

    private func recompute() {
        summary = loadedReport.map { ReportSummary.make(report: $0, settings: settings, rules: rules) }
        updateScope()
        updateInReview()
    }

    private func updateScope() {
        let scope = loadedReport.flatMap { ReportScope.of(report: $0, settings: settings, home: home) }
        // Once a minute and on every open: a write would redraw what reads it.
        if scope != reportScope {
            reportScope = scope
            updateInReview()
        }
    }

    private func updateInReview() {
        let total = AgentsSummary.InReview.make(report: loadedReport, scope: reportScope, settings: settings, rules: rules)
        if total != codingAgentsInReview { codingAgentsInReview = total }
    }

    // MARK: - Scan now

    /// One scan at a time: a second request while one runs waits for it and
    /// asks the runner nothing. The popover, the right-click menu, the Review
    /// window's Scan again and the scheduled path all come here; while paused
    /// none of them starts a scan.
    public func scanNow() async {
        await scan(whilePaused: false)
    }

    /// The rescan after a clean: part of the clean, so it runs while paused too.
    public func scanAfterClean() async {
        await scan(whilePaused: true)
    }

    private func scan(whilePaused: Bool) async {
        // A scheduled scan runs first; this one never joins it.
        if let scheduledTask { _ = await scheduledTask.value }
        if let scanTask {
            await scanTask.value
            return
        }
        guard whilePaused || !paused else {
            log("scan skipped: scans are paused")
            return
        }
        // The rescan after a clean too: the app is about to relaunch.
        guard !updateWaiting else {
            log("scan skipped: an update installs")
            return
        }
        guard let runner else {
            setNotice("cli", "The app's embedded CLI is missing, so it cannot scan. Reinstall DeepDriveCleaner.")
            return
        }
        let task = Task { await runScan(runner) }
        scanTask = task
        await task.value
        scanTask = nil
    }

    private func runScan(_ runner: any CLIRunning) async {
        clearNotice("scan")
        clearNotice("busy")
        scan = ScanProgress(started: clock(), files: nil)
        log("scan started")
        let (lines, sink) = AsyncStream<ProgressEvent.Line>.makeStream()
        let consumer = Task { @MainActor in
            var otherVersion = false
            for await line in lines {
                switch line {
                case .event(.walk(let walk)) where walk.step == .scan:
                    let files = max(scan?.files ?? 0, walk.files)
                    scan?.files = files
                case .otherVersion(let version):
                    if !otherVersion { log("progress events of schema \(version) ignored") }
                    otherVersion = true
                case .event:
                    break
                }
            }
        }
        // On the runner's reader thread: decode and yield, never wait.
        let result = await runner.run(.scan, ["scan", "--json", "--progress", "json"]) { line in
            if let read = ProgressEvent.read(line) { sink.yield(read) }
        }
        sink.finish()
        await consumer.value
        now = clock()
        probeAccess()
        if let message = CLIFailure.message(result.outcome, stderrTail: result.stderrTail) {
            // The last report stays on screen: the CLI replaces the file only on success.
            scan = nil
            // A clean holding the lock is no failure: its notice, without the warning.
            setNotice(result.outcome == .busy ? "busy" : "scan", message)
            log("scan ended \(result.outcome): \(result.stderrTail.suffix(3).joined(separator: " | "))")
            return
        }
        let stdout = result.stdout
        let decoded = await Task.detached { Result { try ScanReport.read(stdout) } }.value
        // Only the app's own scan reads the prompting areas: a report on disk
        // may come from a background scan that left them out.
        if case .success(let read) = decoded {
            notReadable = NotReadable.areas(of: read)
            preferences.setNotReadable(notReadable)
        }
        reportModified = DataDirectory.modified(data.lastScan)
        if DataDirectory.modified(data.settings) != settingsModified { loadSettings() }
        apply(decoded)
        // Only now: until the new report is shown, the scanning state stays,
        // never the state of the report before it.
        scan = nil
        log("scan done")
    }

    // MARK: - The scheduled scan

    public enum ScheduledScan: Sendable, Equatable {
        /// The scan ran; the report on disk is shown.
        case scanned(NotifyDecision)
        /// A clean held the lock: the scan stays due for the next tick.
        case busy
        case failed(String)
    }

    /// `scan --background --notify json`, for the due function's tick. It
    /// waits for a scan the user started and never joins it: `.scan` would
    /// hand it that scan's output, which carries no decision. The popover
    /// shows it as scanning, with the elapsed time only.
    public func scheduledScan() async -> ScheduledScan {
        if let scanTask { await scanTask.value }
        if let scheduledTask { _ = await scheduledTask.value }
        guard let runner else { return .failed("The app's embedded CLI is missing.") }
        let task = Task { await runScheduledScan(runner) }
        scheduledTask = task
        let result = await task.value
        scheduledTask = nil
        return result
    }

    private func runScheduledScan(_ runner: any CLIRunning) async -> ScheduledScan {
        // As a user's scan: a failure's warning lasts until the next scan starts.
        clearNotice("scan")
        clearNotice("busy")
        scan = ScanProgress(started: clock(), files: nil)
        defer { scan = nil }
        log("scheduled scan started")
        // Never `--threshold`: settings.json's threshold wins.
        let result = await runner.run(.backgroundScan, ["scan", "--background", "--notify", "json"])
        now = clock()
        probeAccess()
        if result.outcome == .busy { return .busy }
        if let message = CLIFailure.message(result.outcome, stderrTail: result.stderrTail) {
            setNotice("scan", message)
            log("scheduled scan ended \(result.outcome): \(result.stderrTail.suffix(3).joined(separator: " | "))")
            return .failed(message)
        }
        let decoded: NotifyReport
        do {
            decoded = try NotifyReport.read(result.stdout)
        } catch {
            log("scheduled scan: the decision did not read: \(error)")
            return .failed("The scheduled scan's result could not be read.")
        }
        switch decoded.status {
        case .skipped(let reason):
            log("scheduled scan skipped: \(reason)")
            return .busy
        case .scanned(let decision):
            // The CLI saved the report; the Not readable list stays the app's own scan's.
            if DataDirectory.modified(data.settings) != settingsModified { loadSettings() }
            await loadReport()
            log("scheduled scan done: notify \(decision.notify)")
            return .scanned(decision)
        }
    }

    // MARK: - The review seam

    /// Review…, "See what was found", or a biggest find. `keepingSelection`:
    /// the Whole disk view's Review… link, which leaves the window's selection
    /// as the user made it (`ReviewRequest.keepsSelection`).
    public func review(group: String? = nil, keepingSelection: Bool = false) {
        route(ReviewRequest(group: group, classes: included, confirm: false, keepsSelection: keepingSelection))
    }

    /// Clean selected…: the confirmation sheet, never a clean from here.
    public func cleanSelected() {
        guard reportScope != .outside, !updateWaiting else { return }
        route(ReviewRequest(group: nil, classes: included, confirm: true))
    }

    private func route(_ request: ReviewRequest) {
        if router.open(request) {
            preferences.clearBadge()
            badgePending = false
            clearNotice("review")
        } else {
            setNotice("review", "The Review window could not open.")
        }
    }

    /// Pause or Resume from the menu: while paused, no scan starts, neither
    /// Scan now nor a scheduled one; a scan already running finishes. Kept in
    /// `UserDefaults`, so a relaunch and a `--scheduled` instance see it.
    public func setPaused(_ on: Bool) {
        preferences.setScansPaused(on)
        paused = on
        log(on ? "scans paused" : "scans resumed")
    }

    /// The General pane's "Show the size you can free next to the icon": the icon follows at once.
    public func setShowSize(_ on: Bool) {
        preferences.setShowSize(on)
        showSize = on
    }

    /// The General pane's "Show a badge on the menu bar icon".
    public func setShowBadge(_ on: Bool) {
        preferences.setShowBadge(on)
        showBadge = on
    }

    /// The Review window's clean started or ended, its rescan included.
    public func setCleaning(_ on: Bool) {
        cleaning = on
    }

    /// `UpdateModel.onJobsHeld`.
    public func setUpdateWaiting(_ on: Bool) {
        updateWaiting = on
    }

    /// What the Review window opens on: the report and the rules this popover shows now.
    public var snapshot: ReviewSnapshot? {
        loadedReport.map { ReviewSnapshot(report: $0, rules: rules) }
    }

    /// A notification was posted ("Background scan and notifications in the app").
    public func notificationPosted() {
        preferences.notificationPosted()
        badgePending = true
    }

    /// Remind Me Tomorrow answers the notification: the badge goes.
    public func clearBadge() {
        preferences.clearBadge()
        badgePending = false
    }

    // MARK: - Notices

    /// Dismissing the replace offer declines it: the popover stops offering,
    /// Settings keeps it. Dismissing Allow Notifications drops the flag.
    public func dismiss(_ notice: Notice) {
        notices.removeAll { $0.id == notice.id }
        switch notice.action {
        case .replaceOldScan: preferences.setOldScanOfferDismissed()
        case .allowNotifications: preferences.setNotificationPermissionPending(false)
        case nil: break
        }
    }

    /// A notice's button.
    public func perform(_ action: Notice.Action) {
        onNoticeAction?(action)
    }

    /// A notice from outside the popover (the scheduler agent); nil text withdraws it.
    public func setNotice(id: String, text: String?) {
        if let text { setNotice(id, text) } else { clearNotice(id) }
    }

    private func setNotice(_ id: String, _ text: String, action: Notice.Action? = nil) {
        let notice = Notice(id: id, text: text, action: action)
        if let index = notices.firstIndex(where: { $0.id == id }) {
            notices[index] = notice
        } else {
            notices.append(notice)
        }
    }

    private func clearNotice(_ id: String) {
        notices.removeAll { $0.id == id }
    }
}
