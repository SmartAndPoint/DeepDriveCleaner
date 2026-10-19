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
    /// Why a finding cannot be selected.
    public enum Lock: Sendable, Equatable {
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

        public var text: String {
            switch self {
            case .userData: "Your data · report only"
            case .typeOff: "Its type is switched off in Settings"
            case .underMinimumSize: "Under the minimum size in Settings"
            case .scanRoot: "A scanned folder itself"
            case .noAction: "Manual only: ddc has no command for it"
            case .protected: "Protected by .ddc-keep"
            case .cleaned: "Cleaned just now"
            }
        }
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

        public var selectable: Bool { lock == nil }
        public var riskClass: ScanReport.RiskClass { finding.class }

        /// A `review` item the Trash would take: the sheet offers Delete permanently.
        public var canBePermanent: Bool { finding.class == .review && action == .trash }
    }

    public let report: ScanReport
    public let rows: [Row]
    /// The groups of `rules --json` in display order, then "Other".
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
            return Row(
                id: index,
                finding: finding,
                group: rules?.group(of: finding.rule) ?? ReportSummary.otherGroup,
                action: action,
                lock: lock,
                defaultSelected: decision.selected && lock == nil
            )
        }
        return ReviewItems(report: report, rows: rows, groupOrder: (rules?.groups ?? []) + [ReportSummary.otherGroup])
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

    /// The classes in their order, each with its groups in `rules --json` order.
    public var sidebar: [(riskClass: ScanReport.RiskClass, entries: [Entry])] {
        ScanReport.RiskClass.allCases.compactMap { riskClass in
            let groups = Set(rows.filter { $0.riskClass == riskClass && $0.lock != .typeOff && $0.lock != .underMinimumSize }.map(\.group))
            guard !groups.isEmpty else { return nil }
            let entries = groupOrder.filter(groups.contains).map { Entry(riskClass: riskClass, group: $0) }
            return (riskClass, entries)
        }
    }

    /// The rows of an entry, ignored findings left out (the Items view shows them).
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

    public enum Sort: String, CaseIterable, Sendable {
        case size, path, idle
    }

    /// The Items view: every finding but user data, filtered and sorted.
    public func items(matching query: String, sort: Sort) -> [Row] {
        let rows = self.rows.filter { $0.lock != .userData && matches($0, query) }
        switch sort {
        case .size:
            return rows.sorted { $0.finding.bytes != $1.finding.bytes ? $0.finding.bytes > $1.finding.bytes : $0.id < $1.id }
        case .path:
            return rows.sorted { $0.finding.path != $1.finding.path ? $0.finding.path < $1.finding.path : $0.id < $1.id }
        case .idle:
            // Most idle first; findings outside a project last, biggest first.
            return rows.sorted { a, b in
                let (x, y) = (a.finding.project?.idleDays, b.finding.project?.idleDays)
                if x != y { return (x ?? 0, x != nil ? 1 : 0) > (y ?? 0, y != nil ? 1 : 0) }
                return a.finding.bytes != b.finding.bytes ? a.finding.bytes > b.finding.bytes : a.id < b.id
            }
        }
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
            return Row(id: row.id, finding: row.finding, group: row.group, action: row.action, lock: lock, defaultSelected: false)
        }
        return ReviewItems(report: report, rows: rows, groupOrder: groupOrder)
    }
}
