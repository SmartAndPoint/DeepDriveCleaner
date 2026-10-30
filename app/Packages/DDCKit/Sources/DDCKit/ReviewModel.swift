import Foundation
import Observation

/// The Review window's state (story "Review and clean window"; artboards 4
/// to 6 of the canvas): the findings of one snapshot, the selection, and
/// the confirmation sheet with its warnings, progress and result.
///
/// It is the popover's review router. A request seeds the window and, for
/// Clean selected…, opens the sheet; the clean itself starts only from the
/// sheet (`cleanConfirmedItems()` in `CleanRun.swift`, ADR §7).
@MainActor
@Observable
public final class ReviewModel: ReviewRouting {
    public enum Mode: Sendable, Equatable {
        case groups, items
    }

    /// The dry run behind the sheet's live warnings.
    public enum Warnings: Sendable, Equatable {
        /// Behind another run of the CLI.
        case waiting
        case checking
        case checked
        /// A terminal `ddc` holds the lock.
        case busy
        /// Stopped by the warnings line's Stop, Clean, Check again or closing the sheet.
        case stopped
        case failed(String)
    }

    /// Where a clean is; `Sheet.progress` says how far (story "CLI: progress events").
    public enum Phase: Sendable, Equatable {
        /// Behind another run of the CLI.
        case waiting
        /// Step 1: the CLI's re-check of the listed items, then its keep-marker walk.
        case preparing(since: Date)
        /// Step 2: from the `cleaning` event on, the CLI may touch files.
        case cleaning(since: Date)
        /// Cancel was pressed: the CLI stops after the current item.
        case stopping
    }

    public enum Stage: Sendable, Equatable {
        case confirm
        case cleaning(Phase)
        case result(CleanResult)
        /// The clean did not run, or ended without a report. `journal`: it
        /// may have done something, which the journal lists.
        case stopped(String, journal: URL?)
    }

    public struct Sheet: Sendable, Equatable {
        public internal(set) var confirmation: Confirmation
        public internal(set) var warnings: Warnings
        /// The dry run's prediction per row id.
        public internal(set) var predictions: [Int: Prediction]
        public internal(set) var stage: Stage
        /// The clean's events so far; nil until the first one (a CLI that
        /// writes none leaves the bar indeterminate).
        public internal(set) var progress: CleanProgress?
    }

    /// Nil without a report: the window says to scan first.
    public internal(set) var items: ReviewItems?
    public var selection: Set<Int> = []
    public var mode: Mode = .groups
    /// The detail shows the Whole disk view (story "Whole disk in the app"):
    /// nothing is listed or cleaned there.
    public var wholeDisk = false
    public var entry: ReviewItems.Entry?
    public var query = ""
    /// The rows' order in both views, from `AppPreferences`; `sortBy` changes it.
    public internal(set) var sort: ReviewSort = .default
    /// Folded subgroups of the Groups view, while the window stays open.
    public var collapsed: Set<GroupLayout.Key> = []
    /// Non-nil while the confirmation sheet shows.
    public internal(set) var sheet: Sheet?
    /// The sheet as it was when it closed: drawn while it slides away, so the
    /// dismiss never shows an empty panel.
    public internal(set) var lastSheet: Sheet?
    public internal(set) var notice: String?
    /// Whether the report is of the app's roots (story "Review window: say
    /// when the saved scan is not of the home folder"); nil when
    /// `settings.json` cannot be read. Set when the rows are built.
    public internal(set) var reportScope: ReportScope?
    /// The notice above the groups for a report of other roots, with Scan Now.
    public internal(set) var scopeNotice: String?
    public enum Rescan: Sendable, Equatable {
        /// The popover reads the report the clean saved, or scans after the
        /// clean: the window reloads when it ends.
        case afterClean
        /// Scan again, from the window's header.
        case requested
        /// Remove Protection… took a protection away: the next scan lists
        /// the folder in its own group.
        case afterRemovingProtection
    }

    /// A scan the window waits for; Clean stays off meanwhile.
    public internal(set) var rescanning: Rescan?
    /// A clean, or the rescan after it, is in flight.
    public internal(set) var isCleaning = false
    /// Remove Protection… on a row, until the scan after it ends (story
    /// "Protected by you: say what it is, and Remove Protection").
    public internal(set) var removingProtection: RemoveProtection.Progress?
    /// What the last clean deleted or moved to the Trash: locked in any
    /// report scanned before the clean ended, so they never come back selected.
    @ObservationIgnored var cleaned: (identities: Set<Identity>, ended: Date)?
    /// The last clean's report and when it ended (story "Review window: after
    /// a clean, only skips that can clear stay selected"): the rows' tool
    /// notes read it until a scan newer than the clean replaces the report.
    var lastClean: LastClean?
    /// The reload after the clean has yet to apply its report to the selection.
    @ObservationIgnored var selectAfterClean = false

    @ObservationIgnored let runner: (any CLIRunning)?
    @ObservationIgnored let data: DataDirectory
    @ObservationIgnored let preferences: AppPreferences
    @ObservationIgnored let snapshot: @MainActor () -> ReviewSnapshot?
    @ObservationIgnored let present: @MainActor () -> Bool
    @ObservationIgnored let setBusy: @MainActor (Bool) -> Void
    @ObservationIgnored let rescan: @MainActor () async -> Void
    @ObservationIgnored let reloadReport: @MainActor () async -> Bool
    @ObservationIgnored let paused: @MainActor () -> Bool
    @ObservationIgnored let updateWaiting: @MainActor () -> Bool
    @ObservationIgnored let temporaryDirectory: URL
    @ObservationIgnored let clock: @MainActor () -> Date
    @ObservationIgnored let log: @MainActor (String) -> Void
    @ObservationIgnored let home: String
    @ObservationIgnored let editSettings: @MainActor (_ edit: (inout SettingsFile) -> Void) -> Bool
    @ObservationIgnored let gitTracking: GitTracking
    /// Folders "Protect this folder" marked; locked until a scan reports them as protected.
    @ObservationIgnored private var protected: Set<Identity> = []
    @ObservationIgnored var dryRun: (ticket: CLIRunner.Ticket, task: Task<Void, Never>)?
    @ObservationIgnored var cleanTicket: CLIRunner.Ticket?
    @ObservationIgnored var cleanTask: Task<Void, Never>?
    @ObservationIgnored var quitting = false
    /// The clean's CLI process exists: Quit waits for it, and only for it.
    @ObservationIgnored var cleanProcessLive = false
    @ObservationIgnored var processEnded: [@MainActor () -> Void] = []
    /// `settings.json`'s date when the rows were built; nil before the first build.
    @ObservationIgnored private var settingsModified: Date??
    /// The rules the rows were built with.
    @ObservationIgnored private(set) var builtRules: RuleList?

    /// - `snapshot`: the report and the rules the popover shows now. The
    ///   window reads `settings.json` itself, so a change applies at once.
    /// - `present`: shows the window; false when it could not.
    /// - `setBusy`: the icon's busy input while a clean and the rescan after it run.
    /// - `rescan`: the popover's scan, which runs while paused: it follows a
    ///   clean, and Scan again checks `paused` first.
    /// - `reloadReport`: the popover reads `last-scan.json` again, after a
    ///   clean that saved it; false when it could not be shown.
    /// - `paused`: scans are paused from the menu bar.
    /// - `updateWaiting`: an update's relaunch waits for the runner or runs
    ///   (`UpdateModel.jobsHeld`): no clean, sheet or Scan again starts.
    /// - `editSettings`: `SettingsStore.update`, the one writer of
    ///   `settings.json`, for Remove Protection… on a Settings exclusion or
    ///   skip; false when the file could not be read or written.
    /// - `gitTracking`: whether git tracks a marker, for the confirmation.
    public init(
        runner: (any CLIRunning)?,
        data: DataDirectory,
        preferences: AppPreferences,
        snapshot: @escaping @MainActor () -> ReviewSnapshot?,
        present: @escaping @MainActor () -> Bool,
        setBusy: @escaping @MainActor (Bool) -> Void = { _ in },
        rescan: @escaping @MainActor () async -> Void = {},
        reloadReport: @escaping @MainActor () async -> Bool = { false },
        paused: @escaping @MainActor () -> Bool = { false },
        updateWaiting: @escaping @MainActor () -> Bool = { false },
        temporaryDirectory: URL = FileManager.default.temporaryDirectory,
        clock: @escaping @MainActor () -> Date = { .now },
        log: @escaping @MainActor (String) -> Void = { _ in },
        home: String = FileManager.default.homeDirectoryForCurrentUser.path,
        editSettings: @escaping @MainActor (_ edit: (inout SettingsFile) -> Void) -> Bool = { _ in false },
        gitTracking: GitTracking = GitTracking()
    ) {
        self.editSettings = editSettings
        self.gitTracking = gitTracking
        self.home = home.count > 1 && home.hasSuffix("/") ? String(home.dropLast()) : home
        self.runner = runner
        self.data = data
        self.preferences = preferences
        self.snapshot = snapshot
        self.present = present
        self.setBusy = setBusy
        self.rescan = rescan
        self.reloadReport = reloadReport
        self.paused = paused
        self.updateWaiting = updateWaiting
        self.temporaryDirectory = temporaryDirectory
        self.clock = clock
        self.log = log
        sort = preferences.reviewSort
    }

    // MARK: - The router

    /// Seeds the window from the popover's request and shows it. While the
    /// sheet shows, a request only brings the window forward: the sheet's
    /// items never change behind the user's back.
    public func open(_ request: ReviewRequest) -> Bool {
        if sheet == nil { load(request) }
        guard present() else { return false }
        if request.confirm, sheet == nil { openSheet() }
        return true
    }

    private func load(_ request: ReviewRequest) {
        notice = nil
        wholeDisk = false
        guard let snapshot = snapshot() else {
            items = nil
            selection = []
            entry = nil
            reportScope = nil
            scopeNotice = nil
            return
        }
        let items = make(snapshot)
        self.items = items
        selection = items.defaultSelection(classes: request.classes)
        if let group = request.group {
            mode = .groups
            entry = items.entry(for: group)
        } else {
            let entries = items.sidebar.flatMap(\.entries)
            entry = entries.first { items.rows(in: $0).contains(where: \.defaultSelected) } ?? entries.first
        }
    }

    /// The popover's report again, keeping the selection by `(path, dev, ino)`.
    public func reload() {
        reload(afterClean: false)
    }

    /// `afterClean`: the reload that follows a clean, from `runClean` or the
    /// result's Done. It applies the clean's report once: of the items it
    /// lists, only the skips that can clear and the items it did not reach
    /// stay selected. The other reloads (Settings, Scan Again) leave it pending.
    func reload(afterClean: Bool) {
        guard sheet == nil, let snapshot = snapshot() else { return }
        var kept = Set((items?.rows ?? []).filter { selection.contains($0.id) }.map { Identity($0.finding) })
        if afterClean, selectAfterClean, let report = lastClean?.report {
            selectAfterClean = false
            kept.subtract(report.items.filter { !$0.outcome.staysSelected }.map { Identity(path: $0.path, dev: $0.dev, ino: $0.ino) })
        }
        let entry = self.entry
        let items = make(snapshot)
        self.items = items
        selection = Set(items.rows.filter { $0.selectable && kept.contains(Identity($0.finding)) }.map(\.id))
        let entries = items.sidebar.flatMap(\.entries)
        self.entry = entry.flatMap { entries.contains($0) ? $0 : nil } ?? entries.first
    }

    /// A type switched off or a minimum size raised in Settings applies at
    /// once: when the window becomes key, and before the sheet opens. The
    /// app is the only gate for ignored items.
    public func refreshSettings() {
        guard sheet == nil, items != nil, settingsChanged else { return }
        reload()
    }

    /// Scans are paused from the menu bar: Scan again starts nothing.
    public var scansPaused: Bool { paused() }

    /// Scan again, from the window's header.
    public func scanAgain() async {
        guard rescanning == nil, !isCleaning, !scansPaused, !updateWaiting() else { return }
        rescanning = .requested
        await rescan()
        rescanning = nil
        reload()
    }

    /// Whether `settings.json` changed since the rows were built.
    var settingsChanged: Bool { settingsModified != .some(DataDirectory.modified(data.settings)) }

    private func make(_ snapshot: ReviewSnapshot) -> ReviewItems {
        settingsModified = .some(DataDirectory.modified(data.settings))
        builtRules = snapshot.rules
        // Deleting the marker undoes Protect this folder.
        protected = protected.filter { FileManager.default.fileExists(atPath: String(decoding: $0.path, as: UTF8.self) + "/" + KeepMarker.name) }
        // Unreadable: the report's own flags stand, as in the popover.
        let settings = try? SharedSettings.read(at: data.settings)
        let report = snapshot.report
        reportScope = ReportScope.of(report: report, settings: settings, home: home)
        scopeNotice = reportScope.flatMap { Wording.reportScope($0, roots: report.scanRoots, home: home, reportHome: report.home) }
        var items = ReviewItems.make(report: snapshot.report, settings: settings, rules: snapshot.rules)
        if !protected.isEmpty {
            items = items.locking(Set(items.rows.filter { protected.contains(Identity($0.finding)) }.map(\.id)), as: .protected)
        }
        // A scan that started after the clean ended knows what is gone, and what its tools did.
        let scanned = Wording.date(rfc3339: snapshot.report.generatedAt) ?? .distantPast
        if let lastClean, scanned > lastClean.ended { self.lastClean = nil }
        if let cleaned {
            if scanned > cleaned.ended {
                self.cleaned = nil
            } else {
                items = items.locking(Set(items.rows.filter { cleaned.identities.contains(Identity($0.finding)) }.map(\.id)), as: .cleaned)
            }
        }
        return items
    }

    struct LastClean {
        let report: CleanReport
        let ended: Date
        /// The items whose tool ran, for the rows' notes.
        let tools: [Identity: CleanReport.Item]

        init(report: CleanReport, ended: Date) {
            self.report = report
            self.ended = ended
            var tools: [Identity: CleanReport.Item] = [:]
            for item in report.items {
                if case .native = item.outcome { tools[Identity(path: item.path, dev: item.dev, ino: item.ino)] = item }
            }
            self.tools = tools
        }
    }

    struct Identity: Hashable {
        let path: [UInt8]
        let dev: UInt64
        let ino: UInt64

        init(_ finding: ScanReport.Finding) {
            self.init(path: finding.path, dev: finding.dev, ino: finding.ino)
        }

        init(path: String, dev: UInt64, ino: UInt64) {
            // Bytes, not `String` equality: two spellings of one name stay two.
            self.path = Array(path.utf8)
            self.dev = dev
            self.ino = ino
        }
    }

    /// A row's note when its tool ran in the last clean: "Pruned 3 min ago;
    /// Docker Desktop shrinks its disk image later", or why it failed.
    public func toolNote(for id: Int) -> String? {
        guard let lastClean, let row = items?.row(id), let item = lastClean.tools[Identity(row.finding)] else { return nil }
        return CleanWording.toolNote(item, ended: lastClean.ended, now: clock())
    }

    // MARK: - Selection

    public func isSelected(_ id: Int) -> Bool { selection.contains(id) }

    /// Selectable rows only: user-data findings, locked entries and ignored
    /// findings never enter the selection; a picked entry of the user's own folders does.
    public func toggle(_ id: Int) {
        guard let row = items?.row(id), row.selectable else { return }
        if selection.contains(id) { selection.remove(id) } else { selection.insert(id) }
    }

    /// Selects or clears the selectable rows of `ids`.
    public func set(_ ids: [Int], selected: Bool) {
        guard let items else { return }
        let selectable = ids.filter { items.row($0)?.selectable == true }
        if selected { selection.formUnion(selectable) } else { selection.subtract(selectable) }
    }

    /// Deselect All, in the footer (story "Review window: Deselect All, then
    /// pick across groups and clean only that"): every group's rows, picked
    /// entries and rows the filter hides included. Nothing else changes.
    public func deselectAll() {
        guard canDeselectAll else { return }
        selection = []
    }

    /// Off with nothing selected, and behind the sheet, whose items never
    /// change behind the user's back. A clean or a rescan leaves it on: the
    /// reload after a clean only removes rows from the selection.
    public var canDeselectAll: Bool { !selection.isEmpty && sheet == nil }

    /// The selection as the clean takes it: a finding inside a picked folder
    /// goes to the Trash with the folder, so it is not counted again.
    public var selectedRows: [ReviewItems.Row] {
        guard let items else { return [] }
        let covered = items.covered(by: selection)
        return selection.subtracting(covered).sorted().compactMap { items.row($0) }
    }

    /// Off, mixed or on over the selectable rows of `ids`: Select all and
    /// a subgroup's heading. Off when none of them can be selected.
    public func checkState(_ ids: [Int]) -> CheckState {
        guard let items else { return .off }
        let selectable = ids.filter { items.row($0)?.selectable == true }
        let selected = selectable.filter(selection.contains).count
        return selected == 0 ? .off : (selected == selectable.count ? .on : .mixed)
    }

    /// Whether any of `ids` can be selected: the checkbox is off otherwise.
    public func canSelect(_ ids: [Int]) -> Bool {
        ids.contains { items?.row($0)?.selectable == true }
    }

    /// Select all, or a heading: selects every selectable row of `ids`
    /// unless all are selected, then clears them. Locked rows never change.
    public func toggleAll(_ ids: [Int]) {
        set(ids, selected: checkState(ids) != .on)
    }

    /// The sidebar's and the group header's "selected of total". A
    /// user-data folder's total is its finding; a picked entry counts
    /// without the findings inside it, and never past the total.
    public func amount(of entry: ReviewItems.Entry) -> String {
        guard let items else { return "" }
        if entry.riskClass == .userData, entry.group == CleanWording.cloudFolders, items.rows(in: entry).isEmpty {
            return "not scanned"
        }
        let rows = items.rows(in: entry)
        let total = items.total(entry)
        let selected = rows.filter { selection.contains($0.id) }.reduce(0) { $0 + ($1.userEntry?.ownBytes ?? $1.finding.bytes) }
        return Wording.amount(selected: min(selected, total), total: total)
    }

    /// Whether "Protect this folder" is offered: never for a file, nor
    /// inside a device backup.
    public func canProtect(_ id: Int) -> Bool {
        guard let items, let row = items.row(id), reportScope != .outside else { return false }
        return row.selectable && items.canProtect(row)
    }

    /// A column title of the sort header: sorts by it, or reverses it when
    /// it is the sorted one; kept for the next launch. The selection, Select
    /// all and what a group's Clean button cleans never change with it.
    public func sortBy(_ column: ReviewSort.Column) {
        sort = sort.clicking(column)
        preferences.setReviewSort(sort)
    }

    /// A subgroup's heading: folds it or opens it again.
    public func toggleCollapsed(_ key: GroupLayout.Key) {
        if collapsed.contains(key) { collapsed.remove(key) } else { collapsed.insert(key) }
    }

    /// The window closed: every subgroup opens again next time.
    public func windowClosed() {
        collapsed = []
    }

    public var selectedBytes: UInt64 { selectedRows.reduce(0) { $0 + $1.finding.bytes } }

    /// The selection inside `scope`, the ids a group's Select all row acts
    /// on, as the sheet takes it (story "Review window: clean only the
    /// selection in this group"), so the button, the row's summary and the
    /// sheet count the same rows.
    public func selectedRows(in scope: [Int]) -> [ReviewItems.Row] {
        guard let items else { return [] }
        let picked = selection.intersection(scope)
        return picked.subtracting(items.covered(by: picked)).sorted().compactMap { items.row($0) }
    }

    /// Whether Clean N items… (`scope` nil) or a group's Clean N from… can
    /// open the sheet.
    public func canClean(scope: [Int]?) -> Bool {
        let rows = scope.map(selectedRows(in:)) ?? selectedRows
        return !rows.isEmpty && sheet == nil && rescanning == nil && !isCleaning && reportScope != .outside && !updateWaiting()
    }

    /// A group's Select all row: "2 of 3 · 7.2 GiB".
    public func scopeSummary(_ scope: [Int]) -> String {
        let rows = selectedRows(in: scope)
        let selectable = scope.filter { items?.row($0)?.selectable == true }.count
        return Wording.scopeSummary(selected: rows.count, of: selectable, bytes: rows.reduce(0) { $0 + $1.finding.bytes })
    }

    /// A User data folder's picked row: "2 picked · 3.1 GiB", counted as the
    /// sheet counts them.
    public func pickedSummary(_ scope: [Int]) -> String {
        let rows = selectedRows(in: scope)
        return Wording.picked(count: rows.count, bytes: rows.reduce(0) { $0 + $1.finding.bytes })
    }

    /// "frees about": `native-only` items left out, as `Confirmation.reclaimable` does.
    public var selectedReclaimable: UInt64 {
        selectedRows.filter { $0.action != .nativeOnly }.reduce(0) { $0 + $1.finding.reclaimableBytes }
    }

    /// "Protect this folder", after its confirmation: the marker, then the row
    /// can no longer be selected.
    public func protect(_ id: Int) {
        guard canProtect(id), let row = items?.row(id) else { return }
        do {
            try KeepMarker.protect(path: row.finding.path, dev: row.finding.dev, ino: row.finding.ino)
            protected.insert(Identity(row.finding))
            items = items?.locking([id], as: .protected)
            selection.remove(id)
            notice = nil
            log("protected \(row.finding.path)")
        } catch {
            notice = switch error {
            case .missing: "That folder is gone; nothing was written."
            case .changed: "That folder changed since the scan; nothing was written. Scan Again first."
            case .cannotWrite(EACCES): "That folder is read-only, so the .ddc-keep file could not be written."
            case .cannotWrite(let code): "The .ddc-keep file could not be written (\(String(cString: strerror(code))))."
            }
        }
    }

    // MARK: - The sheet

    /// Clean N items…: the sheet over the selection, and its dry run at once.
    /// A group's Clean N from… passes `scope`, the ids its Select all row
    /// acts on: the sheet then holds the selection inside it only.
    public func openSheet(scope: [Int]? = nil) {
        // Ids are indexes into the report, and the reload below can renumber
        // them: the scope crosses it by identity.
        let scoped = scope.map { ids in Set(ids.compactMap { items?.row($0).map { Identity($0.finding) } }) }
        refreshSettings()
        guard sheet == nil, !isCleaning, rescanning == nil, reportScope != .outside, !updateWaiting(), let items else { return }
        var picked = selection
        if let scoped {
            picked = picked.filter { id in items.row(id).map { scoped.contains(Identity($0.finding)) } ?? false }
        }
        let confirmation = Confirmation(items, selection: picked, deletePermanently: preferences.deleteReviewPermanently)
        guard !confirmation.items.isEmpty else { return }
        sheet = Sheet(confirmation: confirmation, warnings: .waiting, predictions: [:], stage: .confirm, progress: nil)
        checkWarnings()
    }

    public func togglePermanent(_ id: Int) {
        guard sheet?.stage == .confirm else { return }
        sheet?.confirmation.togglePermanent(id)
        // Its prediction was for the other action; Check again gives the new one.
        sheet?.predictions[id] = nil
    }

    /// Back to review, or Done on the result.
    public func closeSheet() {
        guard let sheet else { return }
        switch sheet.stage {
        case .cleaning:
            return
        case .confirm:
            stopWarnings()
            lastSheet = sheet
            self.sheet = nil
        case .result, .stopped:
            lastSheet = sheet
            self.sheet = nil
            if rescanning == nil { reload(afterClean: true) }
        }
    }
}
