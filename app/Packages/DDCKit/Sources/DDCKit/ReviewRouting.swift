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
    /// The Whole disk view's Review… link (story "Review window: coding
    /// agents' data as a group"): a window that is open keeps what the user
    /// selected and only turns to `group`, as a click in its sidebar does.
    /// The link also sits in the window's own Whole disk detail. A closed
    /// window opens with the default selection, as for any request.
    public let keepsSelection: Bool

    public init(group: String?, classes: Set<ScanReport.RiskClass>, confirm: Bool, keepsSelection: Bool = false) {
        self.group = group
        self.classes = classes
        self.confirm = confirm
        self.keepsSelection = keepsSelection
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
