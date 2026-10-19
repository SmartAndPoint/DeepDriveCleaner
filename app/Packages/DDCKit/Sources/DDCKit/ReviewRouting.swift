/// What the popover asks of the Review window: open on a group, with the
/// classes its switches leave on, or straight at the confirmation sheet.
/// The popover never cleans by itself (ADR §7).
public struct ReviewRequest: Sendable, Equatable {
    /// A group of `rules --json`; nil for everything.
    public let group: String?
    /// The classes switched on in the popover; they seed the selection.
    public let classes: Set<ScanReport.RiskClass>
    /// Clean selected…: open the confirmation sheet.
    public let confirm: Bool

    public init(group: String?, classes: Set<ScanReport.RiskClass>, confirm: Bool) {
        self.group = group
        self.classes = classes
        self.confirm = confirm
    }
}

/// The seam to the Review window ("Review and clean window").
@MainActor
public protocol ReviewRouting: AnyObject {
    /// Whether a Review window really opened (the attention badge clears only then).
    func open(_ request: ReviewRequest) -> Bool
}

/// The report and the rules the Review window opens on; the popover hands
/// over what it shows. The window reads `settings.json` itself.
public struct ReviewSnapshot: Sendable, Equatable {
    public let report: ScanReport
    public let rules: RuleList?

    public init(report: ScanReport, rules: RuleList?) {
        self.report = report
        self.rules = rules
    }
}
