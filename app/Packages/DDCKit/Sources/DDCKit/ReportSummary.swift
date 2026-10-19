/// What the popover shows of a scan report (story "Menu bar popover").
///
/// The `ignored` rule is applied again with the current `settings.json`, so a
/// type switched off or a minimum size raised after the scan takes effect at
/// once (ADR §6 item 7). It mirrors `crates/ddc-core/src/report/mod.rs`
/// exactly: a Swift rule that drifts would show totals the CLI does not clean.
public struct ReportSummary: Sendable, Equatable {
    /// `TOMBSTONE` in `crates/ddc-core/src/rules/mod.rs`: ddc's own leftovers
    /// of an interrupted clean are never ignored.
    public static let tombstone = "ddc.tombstone"
    /// The group of a rule `rules --json` does not list.
    public static let otherGroup = "Other"

    public struct ClassRow: Sendable, Equatable {
        public let riskClass: ScanReport.RiskClass
        public let count: Int
        public let bytes: UInt64
        public let selectedCount: Int
        public let selectedBytes: UInt64
        /// What deleting the selected findings frees (`private_bytes`).
        public let selectedReclaimable: UInt64
    }

    public struct Group: Sendable, Equatable {
        public let name: String
        public let selectedBytes: UInt64
        public let selectedCount: Int
        /// Findings of the group that are not ignored.
        public let count: Int
        /// Whether its findings belong to projects ("N idle projects of M").
        public let projects: Bool
    }

    /// Safe, rebuildable, review, user data: the classes the CLI totals.
    public let classes: [ClassRow]
    /// Every finding that is not ignored.
    public let foundBytes: UInt64
    /// What the default selection frees: the CLI's `selected_reclaimable_bytes`,
    /// the number a notification names.
    public let reclaimable: UInt64
    public let ignoredCount: Int
    public let ignoredBytes: UInt64
    /// The biggest groups by default-selected size, without groups that have
    /// nothing selected; empty until `rules --json` has been read.
    public let topGroups: [Group]
    public let threshold: UInt64

    /// As the background scan decides it (`crates/ddc-cli/src/notify.rs`).
    public var isOverThreshold: Bool { reclaimable > 0 && reclaimable >= threshold }

    public func row(_ riskClass: ScanReport.RiskClass) -> ClassRow? {
        classes.first { $0.riskClass == riskClass }
    }

    /// "Selected to clean": the default selection of the classes switched on.
    public func selectedBytes(including included: Set<ScanReport.RiskClass>) -> UInt64 {
        classes.filter { included.contains($0.riskClass) }.reduce(0) { $0 + $1.selectedBytes }
    }

    /// What cleaning the default selection of those classes frees: less than
    /// `selectedBytes` when findings share blocks with copies kept elsewhere.
    public func reclaimable(including included: Set<ScanReport.RiskClass>) -> UInt64 {
        classes.filter { included.contains($0.riskClass) }.reduce(0) { $0 + $1.selectedReclaimable }
    }

    /// `settings` nil: the file could not be read, so the report's own
    /// `ignored` and `selected` stand.
    public static func make(
        report: ScanReport,
        settings: SharedSettings?,
        rules: RuleList?,
        top: Int = 4
    ) -> ReportSummary {
        let decisions = report.findings.map { (finding: $0, decision: decide($0, settings: settings)) }
        let decided = decisions.filter { !$0.decision.ignored }.map { (finding: $0.finding, selected: $0.decision.selected) }
        let ignored = decisions.filter(\.decision.ignored).map(\.finding)

        let classes = ScanReport.RiskClass.allCases.map { riskClass in
            let of = decided.filter { $0.finding.class == riskClass }
            let selected = of.filter(\.selected)
            return ClassRow(
                riskClass: riskClass,
                count: of.count,
                bytes: of.reduce(0) { $0 + $1.finding.bytes },
                selectedCount: selected.count,
                selectedBytes: selected.reduce(0) { $0 + $1.finding.bytes },
                selectedReclaimable: selected.reduce(0) { $0 + $1.finding.reclaimableBytes }
            )
        }

        var topGroups: [Group] = []
        if let rules {
            let order = rules.groups + [otherGroup]
            let byGroup = Dictionary(grouping: decided) { rules.group(of: $0.finding.rule) ?? otherGroup }
            topGroups = byGroup.map { name, members in
                let selected = members.filter(\.selected)
                return Group(
                    name: name,
                    selectedBytes: selected.reduce(0) { $0 + $1.finding.bytes },
                    selectedCount: selected.count,
                    count: members.count,
                    projects: members.contains { $0.finding.project != nil }
                )
            }
            .filter { $0.selectedBytes > 0 }
            .sorted { a, b in
                if a.selectedBytes != b.selectedBytes { return a.selectedBytes > b.selectedBytes }
                return (order.firstIndex(of: a.name) ?? order.count) < (order.firstIndex(of: b.name) ?? order.count)
            }
            .prefix(top)
            .map { $0 }
        }

        return ReportSummary(
            classes: classes,
            foundBytes: classes.reduce(0) { $0 + $1.bytes },
            reclaimable: decided.filter(\.selected).reduce(0) { $0 + $1.finding.reclaimableBytes },
            ignoredCount: ignored.count,
            ignoredBytes: ignored.reduce(0) { $0 + $1.bytes },
            topGroups: topGroups,
            threshold: settings?.threshold ?? SharedSettings.defaultThreshold
        )
    }

    /// The one decision of what a finding is under the current settings: the
    /// popover's totals and the Review window's starting selection both use
    /// it, so "Selected to clean" is the window's selection. `settings` nil:
    /// the report's own flags stand.
    public static func decide(_ finding: ScanReport.Finding, settings: SharedSettings?) -> (ignored: Bool, selected: Bool) {
        let ignored = settings.map { isIgnored(finding, by: $0) } ?? finding.ignored
        if ignored { return (true, false) }
        return (false, settings == nil ? finding.selected : finding.notSelected == nil)
    }

    /// `finding()` in `report/mod.rs`: switched off, or strictly under the minimum size.
    static func isIgnored(_ finding: ScanReport.Finding, by settings: SharedSettings) -> Bool {
        finding.rule != tombstone && (settings.disabledRules.contains(finding.rule) || finding.bytes < settings.minSize)
    }
}
