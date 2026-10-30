import Foundation
import Testing
@testable import DDCKit

/// Story "Review window: Deselect All, then pick across groups and clean only
/// that": the footer's button empties the selection everywhere, and nothing
/// else. Temporary directories and defaults suites only.
@Suite("Review window: Deselect All")
@MainActor
struct DeselectAllTests {
    let data: TempDataDirectory
    let defaults: TempDefaults

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
    }

    private func review(events: ReviewEvents = ReviewEvents()) throws -> ReviewModel {
        let snapshot = try ReviewFixture.snapshot(report: UserDataFixture.report(movies: true), settingsIn: data)
        let review = makeReview(runner: nil, data: data, defaults: defaults, events: events) { snapshot }
        _ = review.open(ReviewRequest(group: nil, classes: [.safe, .rebuildable], confirm: false))
        return review
    }

    private func id(_ review: ReviewModel, _ suffix: String) throws -> Int {
        try #require(review.items?.rows.last { $0.finding.path.hasSuffix(suffix) }).id
    }

    @Test("it clears every group, a picked entry and a row the filter hides; nothing is left to clean")
    func clearsEverything() throws {
        let review = try review()
        let holiday = try id(review, "Holiday.mov")
        review.toggle(holiday)
        review.query = "node_modules"
        let groups = Set(review.selectedRows.map(\.group))
        #expect(groups.count > 1, "the default selection spans several groups")
        #expect(review.isSelected(holiday))
        #expect(review.canDeselectAll)

        review.deselectAll()
        #expect(review.selection.isEmpty)
        #expect(review.selectedRows.isEmpty)
        #expect(!review.canClean(scope: nil))
        #expect(!review.canDeselectAll)
    }

    @Test("then one row in each of two groups: the sheet holds exactly those two")
    func pickAcrossGroups() throws {
        let review = try review()
        review.deselectAll()
        let npm = try id(review, ".npm/_cacache")
        let target = try id(review, "r1/target")
        #expect(review.items?.row(npm)?.group != review.items?.row(target)?.group)
        review.toggle(npm)
        review.toggle(target)
        review.openSheet()
        let confirmation = try #require(review.sheet?.confirmation)
        #expect(Set(confirmation.items.map(\.id)) == [npm, target])
    }

    @Test("it is off behind the confirmation sheet")
    func offBehindTheSheet() throws {
        let review = try review()
        review.openSheet()
        #expect(review.sheet != nil)
        #expect(!review.canDeselectAll)
        review.deselectAll()
        #expect(!review.selection.isEmpty, "the sheet's items never change behind the user's back")
    }

    @Test("the open group, the collapsed subgroups, the filter, the view and the rows stay as they were")
    func changesNothingElse() throws {
        let review = try review()
        let entry = try #require(review.items?.sidebar.flatMap(\.entries).last)
        review.entry = entry
        review.mode = .items
        review.query = "cache"
        let key = GroupLayout.Key(entry: entry, tag: Array("heading".utf8))
        review.toggleCollapsed(key)
        let rows = review.items?.rows

        review.deselectAll()
        #expect(review.entry == entry)
        #expect(review.mode == .items)
        #expect(review.query == "cache")
        #expect(review.collapsed == [key])
        #expect(review.items?.rows == rows)
    }

    @Test("Scan Again keeps the empty selection")
    func scanAgainKeepsIt() async throws {
        let events = ReviewEvents()
        let review = try review(events: events)
        review.deselectAll()
        await review.scanAgain()
        #expect(events.rescans == 1)
        #expect(review.selection.isEmpty)
    }
}
