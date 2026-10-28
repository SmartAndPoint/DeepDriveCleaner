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
        public let id: String
        public let text: String
    }

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
    @ObservationIgnored private let nextScan: @MainActor () -> Date?
    @ObservationIgnored private let clock: @MainActor () -> Date
    @ObservationIgnored private let log: @MainActor (String) -> Void
    @ObservationIgnored private let appVersion: String?
    @ObservationIgnored private var reportModified: Date?
    @ObservationIgnored private var settingsModified: Date?
    /// `settings.json` did not read; the warning lasts until it does, whatever its notice.
    private var settingsUnreadable = false
    @ObservationIgnored private var scanTask: Task<Void, Never>?
    @ObservationIgnored private var started = false

    /// `runner` nil: the embedded CLI is missing. `nextScan` is the scheduler's
    /// next slot ("Background scan and notifications in the app"), nil when
    /// nothing is scheduled.
    public init(
        runner: (any CLIRunning)?,
        data: DataDirectory,
        probe: FullDiskAccessProbe,
        preferences: AppPreferences,
        router: any ReviewRouting,
        nextScan: @escaping @MainActor () -> Date? = { nil },
        appVersion: String? = nil,
        clock: @escaping @MainActor () -> Date = { .now },
        log: @escaping @MainActor (String) -> Void = { _ in }
    ) {
        self.runner = runner
        self.data = data
        self.probe = probe
        self.preferences = preferences
        self.router = router
        self.nextScan = nextScan
        self.appVersion = appVersion
        self.clock = clock
        self.log = log
        now = clock()
        readPreferences()
    }

    // MARK: - What the popover shows

    /// Not without the CLI, and not while paused.
    public var canScan: Bool { runner != nil && !paused }

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

    /// The report's own fact, whatever the probe says now.
    public var withoutAccessNote: String? {
        guard let report = loadedReport, !report.fullDiskAccess else { return nil }
        return "This scan ran without Full Disk Access; part of ~/Library was not read."
    }

    public var nextScanText: String { Wording.nextScan(nextScan(), now: now) }

    /// The scheduler's next slot, as the footer reads it; Settings' Next scan too.
    public var nextScanDate: Date? { nextScan() }

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
        // A clean writes the file when it ends; its reload, or the scan after it, shows it.
        if scan == nil, !cleaning, DataDirectory.modified(data.lastScan) != reportModified { await loadReport() }
    }

    /// The report a clean edited and saved, read at once, during the clean
    /// too (story "Faster clean: re-check only what the selection needs").
    /// False when it cannot be shown: the caller scans instead.
    public func reloadReport() async -> Bool {
        await loadReport()
        return loadedReport != nil
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
        if let scanTask {
            await scanTask.value
            return
        }
        guard whilePaused || !paused else {
            log("scan skipped: scans are paused")
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

    // MARK: - The review seam

    /// Review…, "See what was found", or a biggest find.
    public func review(group: String? = nil) {
        route(ReviewRequest(group: group, classes: included, confirm: false))
    }

    /// Clean selected…: the confirmation sheet, never a clean from here.
    public func cleanSelected() {
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

    /// What the Review window opens on: the report and the rules this popover shows now.
    public var snapshot: ReviewSnapshot? {
        loadedReport.map { ReviewSnapshot(report: $0, rules: rules) }
    }

    /// A notification was posted ("Background scan and notifications in the app").
    public func notificationPosted() {
        preferences.notificationPosted()
        badgePending = true
    }

    // MARK: - Notices

    public func dismiss(_ notice: Notice) {
        notices.removeAll { $0.id == notice.id }
    }

    private func setNotice(_ id: String, _ text: String) {
        let notice = Notice(id: id, text: text)
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
