import Foundation
import Testing
@testable import DDCKit

/// Story "Review window: coding agents' data as a group" (epic DDC-002): the
/// Coding agents group in the Review sidebar, the Whole disk section's
/// Review… link, and the section's total from the last scan's findings.
/// Fixture paths under `/Users/example` only; no harness folder is read.
@Suite("Coding agents in Review")
@MainActor
struct AgentsReviewTests {
    let data: TempDataDirectory
    let defaults: TempDefaults

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
    }

    /// `scan-report-origin`'s scratch (safe, deleted) and transcripts
    /// (review, no command yet), then Claude Code's history (review, to the
    /// Trash) and a sealed folder of its own (user data), which the Review
    /// window shows under Your data. Onto `base`'s own findings, when given.
    static func report(onto base: String? = nil, edit: (inout [[String: Any]]) -> Void = { _ in }) throws -> Data {
        let origin = try #require(try JSONSerialization.jsonObject(with: Fixture.data("scan-report-origin")) as? [String: Any])
        var agents = try #require(origin["findings"] as? [[String: Any]])
        var history = agents[1]
        history["path"] = "/Users/example/.claude/history.jsonl"
        history["rule"] = "agent.claude-code-history"
        history["removal"] = "trash"
        history["bytes"] = 16384
        history["ino"] = 999
        history["hint"] = "Claude Code's prompt history"
        history.removeValue(forKey: "contents")
        var sealed = agents[1]
        sealed["path"] = "/Users/example/.claude/shell-snapshots"
        sealed["rule"] = "agent.claude-code-sealed"
        sealed["class"] = "user-data"
        sealed["rule_class"] = "user-data"
        sealed["bytes"] = 4096
        sealed["ino"] = 998
        sealed["not_selected"] = ["reason": "user-data"]
        sealed.removeValue(forKey: "contents")
        agents += [history, sealed]
        return try Fixture.editedFindings(base ?? "scan-report-origin") { findings in
            findings = base == nil ? agents : findings + agents
            edit(&findings)
        }
    }

    private func items(_ report: Data, settings: SharedSettings? = nil) throws -> ReviewItems {
        ReviewItems.make(report: try ScanReport.read(report), settings: settings, rules: try Fixture.rules())
    }

    private func id(_ items: ReviewItems, _ rule: String) throws -> Int {
        try #require(items.rows.first { $0.finding.rule == rule && $0.userEntry == nil }).id
    }

    /// The popover's report, which a test may replace, as a new scan does.
    final class Shown {
        var snapshot: ReviewSnapshot?
        init(_ snapshot: ReviewSnapshot?) { self.snapshot = snapshot }
    }

    private func snapshot(_ report: Data) throws -> ReviewSnapshot {
        ReviewSnapshot(report: try ScanReport.read(report), rules: try Fixture.rules())
    }

    private func review(_ report: Data) throws -> ReviewModel {
        try review(Shown(snapshot(report)))
    }

    private func review(_ shown: Shown) throws -> ReviewModel {
        makeReview(runner: nil, data: data, defaults: defaults) { shown.snapshot }
    }

    // MARK: - The group in the sidebar

    @Test("the group's label is GROUPS's, and every coding-agent rule has it in rules --json; the sealed ones show under Your data")
    func label() throws {
        let rules = try Fixture.rules()
        #expect(rules.groups.contains(AgentsSummary.group))
        #expect(try rustSource("crates/ddc-core/src/rules/listing.rs").contains("\"\(AgentsSummary.group)\","))
        let agents = rules.rules.filter { $0.id.hasPrefix("agent.") }
        #expect(!agents.isEmpty)
        #expect(agents.allSatisfy { $0.group == AgentsSummary.group })
        #expect(Set(agents.map(\.class)) == [.safe, .review, .userData])
    }

    @Test("Coding agents shows under Safe and under Review, with the group's label, the class's blurb and the catalog's hints")
    func sidebar() throws {
        let items = try items(try Self.report())
        let sections = Dictionary(uniqueKeysWithValues: items.sidebar.map { ($0.riskClass, $0.entries.map(\.group)) })
        #expect(sections[.safe] == [AgentsSummary.group])
        #expect(sections[.review] == [AgentsSummary.group])
        // The sealed folder is the user's own: its own entry under Your data, never in the group.
        #expect(sections[.userData] == ["Claude Code sessions, shell snapshots and plugins"])

        // Safe: one rule, so its rows fall into folders, with what they share said once.
        let safe = items.layout(ReviewItems.Entry(riskClass: .safe, group: AgentsSummary.group), matching: "")
        #expect(safe.shape == .folders)
        #expect(safe.blurb == CleanWording.classBlurb(.safe))
        #expect(safe.facts?.hasPrefix("agent.claude-code-scratch · ") == true)
        #expect(safe.hint == CleanWording.hint("debug logs and legacy folders Claude Code no longer writes"))
        #expect(safe.selectAll == [try id(items, "agent.claude-code-scratch")])

        // Review: a row per harness folder kind, each titled by its rule.
        let review = items.layout(ReviewItems.Entry(riskClass: .review, group: AgentsSummary.group), matching: "")
        #expect(review.shape == .rules)
        #expect(review.blurb == CleanWording.classBlurb(.review))
        let singles = review.blocks.compactMap { if case .single(let single) = $0 { single } else { nil } }
        #expect(Set(singles.map(\.title)) == ["claude-code-history", "claude-code-transcripts"])
        #expect(review.count == 2)
        #expect(review.bytes == 49152 + 16384)
        #expect(items.total(ReviewItems.Entry(riskClass: .review, group: AgentsSummary.group)) == 49152 + 16384)
    }

    @Test("its rows select and clean like any group's: scratch deleted, history to the Trash, a folder with no command locked")
    func selectAndClean() throws {
        let review = try review(try Self.report())
        #expect(review.open(ReviewRequest(group: AgentsSummary.group, classes: [.safe], confirm: false)))
        let items = try #require(review.items)
        let scratch = try id(items, "agent.claude-code-scratch")
        let history = try id(items, "agent.claude-code-history")
        let transcripts = try id(items, "agent.claude-code-transcripts")
        #expect(review.entry == ReviewItems.Entry(riskClass: .safe, group: AgentsSummary.group))
        #expect(review.selection == [scratch])
        #expect(items.row(transcripts)?.lock == .noAction)
        #expect(!review.canSelect([transcripts]))

        // Select all of the Review entry takes the history only.
        let entry = ReviewItems.Entry(riskClass: .review, group: AgentsSummary.group)
        let scope = try #require(items.layout(entry, matching: "").selectAll)
        review.toggleAll(scope)
        #expect(review.selection == [scratch, history])
        #expect(review.amount(of: entry) == Wording.amount(selected: 16384, total: 49152 + 16384))

        review.openSheet()
        let confirmation = try #require(review.sheet?.confirmation)
        #expect(confirmation.items.map(\.finding.rule) == ["agent.claude-code-scratch", "agent.claude-code-history"])
        #expect(confirmation.items.map(\.effectiveAction) == [.delete, .trash])
        review.closeSheet()

        // The group's own Clean takes only what is selected in it.
        review.openSheet(scope: scope)
        #expect(review.sheet?.confirmation.items.map(\.id) == [history])
    }

    // MARK: - The Review… link

    private static let link = ReviewRequest(group: AgentsSummary.group, classes: [.safe, .rebuildable], confirm: false, keepsSelection: true)

    @Test("the Review… link turns an open window to Coding agents and keeps what the user selected, across a newer report")
    func linkKeepsSelection() throws {
        let shown = Shown(try snapshot(try Self.report(onto: "scan-report-groups")))
        let review = try review(shown)
        // The popover's Review…: the window lands on another group.
        _ = review.open(ReviewRequest(group: "node_modules", classes: [.safe, .rebuildable], confirm: false))
        let items = try #require(review.items)
        #expect(review.entry?.group == "node_modules")
        let history = try id(items, "agent.claude-code-history")
        review.toggle(history)
        review.toggle(try ReviewFixture.id(items, "p1/node_modules"))
        let picked = Set(review.selection.compactMap { items.row($0)?.finding.path })
        // The link sits in the window's Whole disk detail too.
        review.wholeDisk = true
        review.mode = .items
        // A scan since: the same folders, other sizes and other ids.
        shown.snapshot = try snapshot(try Self.report(onto: "scan-report-groups") { findings in
            // The history moves to the front.
            findings.swapAt(0, findings.count - 2)
            findings[1]["bytes"] = 123_456
        })

        #expect(review.open(Self.link))
        let after = try #require(review.items)
        #expect(try id(after, "agent.claude-code-history") != history)
        #expect(review.entry == ReviewItems.Entry(riskClass: .safe, group: AgentsSummary.group))
        #expect(!review.wholeDisk)
        #expect(review.mode == .groups)
        #expect(Set(review.selection.compactMap { after.row($0)?.finding.path }) == picked)
        #expect(review.sheet == nil)
    }

    @Test("the Review… link on a window not open yet, or closed since, opens it on Coding agents with the default selection")
    func linkOpensFresh() throws {
        let review = try review(try Self.report(onto: "scan-report-groups"))
        #expect(review.open(Self.link))
        let items = try #require(review.items)
        #expect(review.entry == ReviewItems.Entry(riskClass: .safe, group: AgentsSummary.group))
        #expect(review.selection == items.defaultSelection(classes: [.safe, .rebuildable]))

        review.toggle(try id(items, "agent.claude-code-history"))
        review.windowClosed()
        #expect(review.open(Self.link))
        #expect(review.selection == items.defaultSelection(classes: [.safe, .rebuildable]))
    }

    @Test("the Review… link changes nothing behind the sheet, and leaves the Whole disk detail only for a group the report holds")
    func linkLeavesWindowAlone() throws {
        let shown = Shown(try snapshot(try Self.report(onto: "scan-report-groups")))
        let review = try review(shown)
        _ = review.open(ReviewRequest(group: "node_modules", classes: [.safe, .rebuildable], confirm: false))
        review.openSheet()
        let sheet = try #require(review.sheet)
        let entry = review.entry
        #expect(review.open(Self.link))
        #expect(review.sheet == sheet)
        #expect(review.entry == entry)
        review.closeSheet()

        // A newer scan found no coding agents' data: the window stays on Whole disk.
        review.wholeDisk = true
        shown.snapshot = try snapshot(try Fixture.data("scan-report-groups"))
        #expect(review.open(Self.link))
        #expect(review.wholeDisk)
        #expect(review.items?.rows.contains { $0.group == AgentsSummary.group } == false)
    }

    @Test("the popover hands the seam the link's request: the group, the classes switched on, the selection kept")
    func linkRequest() throws {
        let router = FakeRouter()
        let model = try makeModel(runner: nil, data: data, defaults: defaults, router: router)
        model.review(group: AgentsSummary.group, keepingSelection: true)
        #expect(router.requests == [
            ReviewRequest(group: AgentsSummary.group, classes: [.safe, .rebuildable], confirm: false, keepsSelection: true),
        ])
    }

    // MARK: - The section's total

    @Test("the section's total counts the group's findings in every class, as the sidebar does")
    func total() throws {
        let report = try ScanReport.read(try Self.report())
        let total = try #require(AgentsSummary.InReview.make(report: report, scope: .same, settings: nil, rules: try Fixture.rules()))
        // Scratch, transcripts and history; the sealed folder is Your data.
        #expect(total == AgentsSummary.InReview(bytes: 8192 + 49152 + 16384, folders: 3))
        #expect(total.line == "In Review: \(ByteSize.human(8192 + 49152 + 16384)) in 3 folders")
        let items = try items(try Self.report())
        let sidebar = [ScanReport.RiskClass.safe, .review].map { items.total(ReviewItems.Entry(riskClass: $0, group: AgentsSummary.group)) }
        #expect(total.bytes == sidebar.reduce(0, +))
        // A report of folders inside the app's roots still counts.
        #expect(AgentsSummary.InReview.make(report: report, scope: .inside, settings: nil, rules: try Fixture.rules()) == total)

        // A type switched off in Settings leaves the group, and the total, at once.
        var settings = SharedSettings()
        settings.disabledRules = ["agent.claude-code-scratch"]
        let off = try #require(AgentsSummary.InReview.make(report: report, scope: .same, settings: settings, rules: try Fixture.rules()))
        #expect(off == AgentsSummary.InReview(bytes: 49152 + 16384, folders: 2))
        #expect(off.bytes == (try self.items(try Self.report(), settings: settings)).total(ReviewItems.Entry(riskClass: .review, group: AgentsSummary.group)))

        let one = try ScanReport.read(try Self.report { $0 = [$0[0]] })
        #expect(AgentsSummary.InReview.make(report: one, scope: nil, settings: nil, rules: try Fixture.rules())?.line
            == "In Review: \(ByteSize.human(8192)) in 1 folder")
    }

    @Test("the popover keeps the section's total with its report, its settings and its rules")
    func popoverTotal() async throws {
        try data.write(lastScan: try Self.report())
        try data.write(settings: #"{"schema_version": 1, "disabled_rules": ["agent.claude-code-scratch"]}"#)
        let runner = FakeRunner(scan: FakeRunner.done(Data()), rules: FakeRunner.done(try Fixture.data("rules")))
        let model = try makeModel(runner: runner, data: data, defaults: defaults)
        #expect(model.codingAgentsInReview == nil)
        await model.start()
        #expect(model.reportScope == .same)
        #expect(model.codingAgentsInReview == AgentsSummary.InReview(bytes: 49152 + 16384, folders: 2))
    }

    @Test("no total without a report, for a report of another home or outside the app's roots, without rules, or with nothing in the group")
    func noTotal() throws {
        let rules = try Fixture.rules()
        let report = try ScanReport.read(try Self.report())
        #expect(AgentsSummary.InReview.make(report: nil, scope: nil, settings: nil, rules: rules) == nil)
        #expect(AgentsSummary.InReview.make(report: report, scope: .outside, settings: nil, rules: rules) == nil)
        // Another home is foreign whatever the settings.
        let foreign = ReportScope.of(report: report, settings: nil, home: "/Users/someone-else")
        #expect(foreign == .outside)
        #expect(AgentsSummary.InReview.make(report: report, scope: foreign, settings: nil, rules: rules) == nil)
        #expect(AgentsSummary.InReview.make(report: report, scope: .same, settings: nil, rules: nil) == nil)
        #expect(AgentsSummary.InReview.make(report: try Fixture.groupsReport(), scope: .same, settings: nil, rules: rules) == nil)
    }
}
