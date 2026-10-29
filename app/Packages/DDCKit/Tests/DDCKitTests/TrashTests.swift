import Foundation
import Testing
@testable import DDCKit

/// Story "The Trash: its own row, not Your data": the Trash is its own
/// sidebar entry, never selected nor counted in a "frees" total, and a clean
/// says what went there.
@Suite("Review window: the Trash")
struct TrashTests {
    private static let entry = ReviewItems.Entry(riskClass: .review, group: "Trash")

    /// The groups report with a Trash of 4 MiB, as the scan reports it.
    private func report() throws -> Data {
        let trash = try ReviewFixture.finding(5, [
            "path": "/Users/example/.Trash", "rule": "trash", "category": "trash", "class": "review", "rule_class": "review",
            "removal": "none", "bytes": 4 << 20, "files": 3, "dirs": 1, "project": NSNull(), "selected": false,
            "not_selected": ["reason": "review"], "hint": "empty the Trash in Finder", "ino": 9_100,
        ])
        return try Fixture.editedFindings("scan-report-groups") { $0.append(trash) }
    }

    @Test("the Trash is one sidebar entry named Trash in the review section, and its row cannot be selected")
    func sidebar() throws {
        let items = try ReviewFixture.items(report: try report())
        let review = try #require(items.sidebar.first { $0.riskClass == .review })
        #expect(review.entries.filter { $0 == Self.entry }.count == 1)
        #expect(items.sidebar.flatMap(\.entries).filter { $0.group == "Trash" }.count == 1)
        #expect(!items.sidebar.flatMap(\.entries).contains { $0.group == "Your data" && items.rows(in: $0).contains { $0.finding.rule == "trash" } })
        let row = try #require(items.rows.first { $0.finding.rule == ReviewItems.trashRule })
        #expect(row.group == "Trash" && !row.selectable && !row.defaultSelected)
    }

    @Test("its group says what the Trash is, opens it, and has no Select all and no repeated hint")
    func layout() throws {
        let layout = try ReviewFixture.items(report: try report()).layout(Self.entry, matching: "")
        #expect(layout.blurb == CleanWording.trashBlurb)
        #expect(layout.opensTrash && layout.selectAll == nil && layout.hint == nil)
        let other = try ReviewFixture.items(report: try report()).layout(ReviewItems.Entry(riskClass: .review, group: "Python environments"), matching: "")
        #expect(!other.opensTrash)
    }

    @Test("the Trash is in no reclaimable total, review included")
    func totals() throws {
        let classes: Set<ScanReport.RiskClass> = [.safe, .rebuildable, .review]
        let with = ReportSummary.make(report: try ScanReport.read(try report()), settings: try Fixture.groupsSettings(), rules: try Fixture.rules())
        let without = ReportSummary.make(report: try Fixture.groupsReport(), settings: try Fixture.groupsSettings(), rules: try Fixture.rules())
        #expect(with.reclaimable(including: classes) == without.reclaimable(including: classes))
    }
}
