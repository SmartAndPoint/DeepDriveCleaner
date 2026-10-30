import Foundation

/// The Review window's findings, built once per snapshot of the report, the
/// settings and the rules (artboard 4 of the canvas).
///
/// Which findings can be selected mirrors `build_plan` in
/// `crates/ddc-core/src/plan.rs`: not user data, not ignored under the current
/// settings, not a scan root, and an action ddc can take. The app is the only
/// gate for ignored findings: `clean --items` never drops a listed item for
/// its type (ADR §6 item 1).
public struct ReviewItems: Sendable, Equatable {
    /// `TRASH_RULE` in `crates/ddc-core/src/report/after_clean.rs`: the Trash,
    /// report only, in a group of its own.
    public static let trashRule = "trash"

    /// Why a finding cannot be selected.
    public enum Lock: Sendable, Equatable {
        /// A user-data finding: shown, never cleaned.
        case userData
        case typeOff
        case underMinimumSize
        case scanRoot
        /// ddc has no command for it (`native-only` with no commands, or `none`).
        case noAction
        /// "Protect this folder" put a `.ddc-keep` in it since the scan.
        case protected
        /// The last clean deleted it or moved it to the Trash; the next scan drops it.
        case cleaned
        /// An entry of the user's own folders that cannot be picked.
        case entry(EntryLock)

        public var text: String {
            switch self {
            case .userData: "Your data · shown, not cleaned"
            case .typeOff: "Its type is switched off in Settings"
            case .underMinimumSize: "Under the minimum size in Settings"
            case .scanRoot: "A scanned folder itself"
            case .noAction: "Manual only: ddc has no command for it"
            case .protected: "Protected by .ddc-keep"
            case .cleaned: "Cleaned just now"
            case .entry(let lock): CleanWording.entryLock(lock)
            }
        }
    }

    /// A file or folder of one of the user's own folders (story "User data:
    /// pick your own files for the Trash"), picked one by one.
    public struct UserEntry: Sendable, Equatable {
        public let kind: ScanReport.FolderEntry.Kind
        /// The row of its folder.
        public let folder: Int
        /// Its bytes without the findings inside it, which have rows of their own.
        public let ownBytes: UInt64
        public let modified: String?
        public let findings: UInt64
        public let git: Bool
        public let backup: ScanReport.Backup?
    }

    public struct Row: Identifiable, Sendable, Equatable {
        /// The finding's index in the report: rows are never keyed by path,
        /// whose `String` equality would merge two spellings of one name.
        public let id: Int
        public let finding: ScanReport.Finding
        /// Its group of `rules --json`, or "Other".
        public let group: String
        /// What the clean does with it when it is not deleted permanently.
        public let action: ListedAction?
        public let lock: Lock?
        /// Part of "Selected to clean" in the popover.
        public let defaultSelected: Bool
        /// Set on an entry of the user's own folders; its finding is synthesized.
        public let userEntry: UserEntry?

        public var selectable: Bool { lock == nil }
        public var riskClass: ScanReport.RiskClass { finding.class }

        /// A `review` item the Trash would take: the sheet offers Delete permanently.
        public var canBePermanent: Bool { finding.class == .review && action == .trash }
    }

    public let report: ScanReport
    public let rows: [Row]
    /// The groups of `rules --json` in display order, then "Other", then the
    /// user-data rules' own names.
    public let groupOrder: [String]

    public static func make(report: ScanReport, settings: SharedSettings?, rules: RuleList?) -> ReviewItems {
        let switchable = Dictionary(rules?.rules.map { ($0.id, $0.switchable) } ?? [], uniquingKeysWith: { a, _ in a })
        let rows = report.findings.enumerated().map { index, finding in
            let decision = ReportSummary.decide(finding, settings: settings)
            let action = ListedAction.derive(finding, switchable: switchable[finding.rule], permanent: false)
            let lock: Lock? =
                if finding.class == .userData || finding.ruleClass == .userData {
                    .userData
                } else if decision.ignored {
                    ignoredLock(finding, settings: settings)
                } else if report.scanRoots.contains(finding.path) {
                    .scanRoot
                } else if action == nil {
                    .noAction
                } else {
                    nil
                }
            let group = finding.class == .userData ? CleanWording.userDataName(finding.rule) : nil
            return Row(
                id: index,
                finding: finding,
                group: group ?? rules?.group(of: finding.rule) ?? ReportSummary.otherGroup,
                action: action,
                lock: lock,
                defaultSelected: decision.selected && lock == nil,
                userEntry: nil
            )
        }
        let names = userDataOrder.compactMap(CleanWording.userDataName)
        return ReviewItems(
            report: report,
            rows: rows + entryRows(of: rows, report: report),
            groupOrder: (rules?.groups ?? []) + [ReportSummary.otherGroup] + names
        )
    }

    /// The user-data rules in the sidebar's order of ties: `listing.rs`.
    static let userDataOrder = [
        "user.kept", "user.pictures", "user.movies", "user.music", "user.downloads", "user.ios-backups",
        "user.imazing-backups", "user.telegram", "user.whatsapp", "user.cloud", "xcode.archives",
        "ide.workspace-storage", "macos.app-bundle", "node.global-packages",
        "agent.claude-code-sealed", "agent.cursor-sealed",
    ]

    /// The listed entries of each folder that shows, ids after the findings'.
    /// Each is to the Trash, never selected by default, locked by its own lock.
    private static func entryRows(of rows: [Row], report: ScanReport) -> [Row] {
        var out: [Row] = []
        for folder in rows where folder.lock == .userData {
            guard let contents = folder.finding.contents else { continue }
            for entry in contents.entries {
                // Findings inside it have rows of their own: its own bytes leave them out.
                let nested: UInt64 =
                    entry.findings == 0
                    ? 0
                    : report.findings.lazy.filter { Self.inside($0.path, entry.path) }.reduce(0) { $0 + $1.bytes }
                out.append(Row(
                    id: rows.count + out.count,
                    finding: ScanReport.Finding(entry: entry, folder: folder.finding),
                    group: folder.group,
                    action: .trash,
                    lock: entry.lock.map(Lock.entry),
                    defaultSelected: false,
                    userEntry: UserEntry(
                        kind: entry.kind,
                        folder: folder.id,
                        ownBytes: entry.bytes - min(entry.bytes, nested),
                        modified: entry.modified,
                        findings: entry.findings,
                        git: entry.git,
                        backup: entry.backup
                    )
                ))
            }
        }
        return out
    }

    /// `path` lies strictly inside `folder`, compared as bytes.
    static func inside(_ path: String, _ folder: String) -> Bool {
        let (p, f) = (Array(path.utf8), Array(folder.utf8))
        return p.count > f.count + 1 && p.starts(with: f) && p[f.count] == UInt8(ascii: "/")
    }

    private static func ignoredLock(_ finding: ScanReport.Finding, settings: SharedSettings?) -> Lock {
        guard let settings else { return .typeOff }
        return settings.disabledRules.contains(finding.rule) ? .typeOff : .underMinimumSize
    }

    public func row(_ id: Int) -> Row? {
        rows.indices.contains(id) ? rows[id] : nil
    }

    /// The default selection of `classes`: the popover's "Selected to clean".
    public func defaultSelection(classes: Set<ScanReport.RiskClass>) -> Set<Int> {
        Set(rows.filter { $0.defaultSelected && classes.contains($0.riskClass) }.map(\.id))
    }

    // MARK: - Sidebar

    /// One entry of the sidebar: a group within a class.
    public struct Entry: Hashable, Sendable {
        public let riskClass: ScanReport.RiskClass
        public let group: String
    }

    /// The classes in their order, each with its groups largest first (story
    /// "Review window: sorted sidebar, Select all and folder groups"), ties in
    /// `rules --json` order. By total, never by the selection, so an entry
    /// does not move while the user ticks.
    ///
    /// User data has one entry per rule, and "Cloud folders" last when the
    /// scan skipped them: it has no finding, only the report's skipped areas.
    public var sidebar: [(riskClass: ScanReport.RiskClass, entries: [Entry])] {
        ScanReport.RiskClass.allCases.compactMap { riskClass in
            let groups = Set(rows.filter { $0.riskClass == riskClass && $0.lock != .typeOff && $0.lock != .underMinimumSize }.map(\.group))
            let cloud = riskClass == .userData && !cloudSkipped.isEmpty ? [Entry(riskClass: riskClass, group: CleanWording.cloudFolders)] : []
            guard !groups.isEmpty || !cloud.isEmpty else { return nil }
            let ordered = groupOrder.filter(groups.contains).map { Entry(riskClass: riskClass, group: $0) }
            var totals: [Entry: UInt64] = [:]
            for entry in ordered { totals[entry] = total(entry) }
            // Equal totals keep `rules --json` order.
            let entries = ordered.enumerated().sorted { a, b in
                let (x, y) = (totals[a.element] ?? 0, totals[b.element] ?? 0)
                return x != y ? x > y : a.offset < b.offset
            }.map(\.element)
            return (riskClass, entries.filter { $0.group != CleanWording.cloudFolders } + cloud)
        }
    }

    /// The cloud folders the scan did not enter (`excluded`), by path.
    public var cloudSkipped: [String] {
        report.skipped.filter { $0.reason == .excluded && Self.isCloud($0.path, home: report.home) }.map(\.path)
    }

    /// A background scan skipped the areas macOS guards with a prompt: an
    /// `excluded` area that is neither a cloud folder nor the user's skip.
    public var backgroundSkipped: Bool {
        report.skipped.contains { skipped in
            skipped.reason == .excluded && !Self.isCloud(skipped.path, home: report.home)
                && !report.notScanned.contains { skipped.path == $0 || Self.inside(skipped.path, $0) }
        }
    }

    /// The Review window's "Not scanned" line: the user's skips the scan
    /// stopped at; nil when there are none.
    public var notScanned: String? {
        guard !report.notScanned.isEmpty else { return nil }
        return CleanWording.notScanned(report.notScanned.map { Wording.tilde($0, home: report.home) })
    }

    static func isCloud(_ path: String, home: String) -> Bool {
        ["Library/CloudStorage", "Library/Mobile Documents"].contains { area in
            let folder = (home.hasSuffix("/") ? home : home + "/") + area
            return path == folder || inside(path, folder)
        }
    }

    /// Selected findings inside a selected folder entry: the entry takes them
    /// to the Trash, so they are neither counted nor sent on their own.
    public func covered(by selection: Set<Int>) -> Set<Int> {
        let folders = selection.compactMap { row($0) }.filter { $0.userEntry?.kind == .folder && $0.selectable }.map(\.finding.path)
        guard !folders.isEmpty else { return [] }
        return Set(selection.filter { id in
            guard let row = row(id), row.userEntry == nil else { return false }
            return folders.contains { Self.inside(row.finding.path, $0) }
        })
    }

    /// "Protect this folder": a folder finding, or a folder entry outside a
    /// device backup (a stray file there is not known to be harmless).
    public func canProtect(_ row: Row) -> Bool {
        guard let entry = row.userEntry else { return true }
        return entry.kind == .folder && row.finding.rule != "user.ios-backups"
    }

    /// The rows of an entry, ignored findings left out (the Items view shows
    /// them); a user-data folder's entries included.
    public func rows(in entry: Entry) -> [Row] {
        rows.filter { $0.riskClass == entry.riskClass && $0.group == entry.group && $0.lock != .typeOff && $0.lock != .underMinimumSize }
    }

    /// Where a group request from the popover lands: the first entry in class
    /// order that holds a default-selected finding of the group, else the
    /// first entry of the group at all.
    public func entry(for group: String) -> Entry? {
        let entries = sidebar.flatMap(\.entries).filter { $0.group == group }
        return entries.first { entry in rows(in: entry).contains(where: \.defaultSelected) } ?? entries.first
    }

    // MARK: - Search and sort

    /// The Items view: every finding but user data, filtered, then sorted.
    /// Its Name is the path as the row shows it, under `~`.
    public func items(matching query: String, sort: ReviewSort) -> [Row] {
        let rows = self.rows.filter { $0.lock != .userData && $0.userEntry == nil && matches($0, query) }
        return sort.sorted(rows, key: { row in
            ReviewSort.Key(name: Wording.tilde(row.finding.path, home: report.home), bytes: row.finding.bytes, changed: row.finding.modified)
        }, tie: { $0.id < $1.id })
    }

    /// The path, also as written under `~`, or the rule id; case-insensitive.
    public func matches(_ row: Row, _ query: String) -> Bool {
        let query = query.trimmingCharacters(in: .whitespaces)
        guard !query.isEmpty else { return true }
        return row.finding.path.localizedCaseInsensitiveContains(query)
            || Wording.tilde(row.finding.path, home: report.home).localizedCaseInsensitiveContains(query)
            || row.finding.rule.localizedCaseInsensitiveContains(query)
    }

    /// A copy where the selectable rows of `ids` are locked: protected since
    /// the scan, or cleaned since.
    public func locking(_ ids: Set<Int>, as lock: Lock) -> ReviewItems {
        let rows = self.rows.map { row in
            guard ids.contains(row.id), row.lock == nil else { return row }
            return Row(
                id: row.id, finding: row.finding, group: row.group, action: row.action, lock: lock, defaultSelected: false,
                userEntry: row.userEntry
            )
        }
        return ReviewItems(report: report, rows: rows, groupOrder: groupOrder)
    }
}
