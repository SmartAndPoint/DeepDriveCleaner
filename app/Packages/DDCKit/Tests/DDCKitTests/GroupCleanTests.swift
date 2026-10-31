import Foundation
import Testing
@testable import DDCKit

/// Story "Review window: clean only the selection in this group": a group's
/// Clean N from… opens the sheet on the selection inside the rows its Select
/// all row acts on. Temporary directories and defaults suites only.
@Suite("Review window: clean only this group")
@MainActor
struct GroupCleanTests {
    let data: TempDataDirectory
    let defaults: TempDefaults

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
    }

    private func review() throws -> ReviewModel {
        let snapshot = try ReviewFixture.snapshot(settingsIn: data)
        let review = makeReview(runner: nil, data: data, defaults: defaults) { snapshot }
        _ = review.open(ReviewRequest(group: nil, classes: [.safe, .rebuildable], confirm: false))
        return review
    }

    /// The ids a group's Select all row acts on.
    private func scope(_ review: ReviewModel, _ riskClass: ScanReport.RiskClass, _ group: String, matching query: String = "") throws -> [Int] {
        let items = try #require(review.items)
        return try #require(items.layout(ReviewItems.Entry(riskClass: riskClass, group: group), matching: query).selectAll)
    }

    @Test("a group's sheet holds only that group's selected items; the footer's still holds every group's")
    func scoped() throws {
        let review = try review()
        let items = try #require(review.items)
        let chrome = try ReviewFixture.id(items, "Google/Chrome")
        let p1 = try ReviewFixture.id(items, "p1/node_modules")
        review.selection = [chrome, p1]

        review.openSheet(scope: try scope(review, .safe, "Browser and app caches"))
        let confirmation = try #require(review.sheet).confirmation
        #expect(confirmation.items.map(\.id) == [chrome])
        #expect(confirmation.bytes == 303_104)
        review.closeSheet()
        #expect(review.selection == [chrome, p1], "the sheet takes nothing out of the selection")

        review.openSheet()
        #expect(Set(try #require(review.sheet).confirmation.items.map(\.id)) == [chrome, p1])
    }

    @Test("the scope crosses the reload before the sheet by identity: a newer report with its rows in another order opens the same items")
    func scopeByIdentity() throws {
        var json = try #require(try JSONSerialization.jsonObject(with: Fixture.data("scan-report-groups")) as? [String: Any])
        json["findings"] = Array(try #require(json["findings"] as? [Any]).reversed())
        let reordered = ReviewSnapshot(report: try ScanReport.read(try JSONSerialization.data(withJSONObject: json)), rules: try Fixture.rules())
        var current = try ReviewFixture.snapshot(settingsIn: data)
        let review = makeReview(runner: nil, data: data, defaults: defaults) { current }
        _ = review.open(ReviewRequest(group: nil, classes: [.safe, .rebuildable], confirm: false))
        let items = try #require(review.items)
        let chrome = try ReviewFixture.id(items, "Google/Chrome")
        review.selection = [chrome, try ReviewFixture.id(items, "p1/node_modules")]
        let apps = try scope(review, .safe, "Browser and app caches")

        // A newer report, and a settings change that makes the sheet reload first.
        current = reordered
        try FileManager.default.setAttributes([.modificationDate: Date().addingTimeInterval(60)], ofItemAtPath: data.directory.settings.path)
        review.openSheet(scope: apps)

        #expect(try ReviewFixture.id(try #require(review.items), "Google/Chrome") != chrome, "the rows were renumbered")
        let confirmation = try #require(review.sheet).confirmation
        #expect(confirmation.items.map(\.finding.path) == [try #require(items.row(chrome)).finding.path])
    }

    @Test("with a filter typed, the button acts on the rows the filter shows, as Select all does")
    func filtered() throws {
        let review = try review()
        let items = try #require(review.items)
        let p1 = try ReviewFixture.id(items, "p1/node_modules")
        let p2 = try ReviewFixture.id(items, "p2/node_modules")
        review.selection = [p1, p2]
        let shown = try scope(review, .rebuildable, "node_modules", matching: "p1")
        #expect(shown.contains(p1) && !shown.contains(p2))

        review.openSheet(scope: shown)
        #expect(try #require(review.sheet).confirmation.items.map(\.id) == [p1])
    }

    @Test("a finding inside a picked folder of your own files goes with its group's clean, and the button counts what the sheet holds")
    func insidePickedFolder() throws {
        let snapshot = ReviewSnapshot(report: try ScanReport.read(UserDataFixture.report()), rules: try Fixture.rules())
        let review = makeReview(runner: nil, data: data, defaults: defaults) { snapshot }
        _ = review.open(ReviewRequest(group: nil, classes: [], confirm: false))
        let items = try #require(review.items)
        let project = try ReviewFixture.id(items, "Downloads/Project")
        let nested = try ReviewFixture.id(items, "Project/node_modules")
        let p1 = try ReviewFixture.id(items, "p1/node_modules")
        review.selection = [project, nested, p1]
        let modules = try scope(review, .rebuildable, "node_modules")

        let rows = review.selectedRows(in: modules)
        #expect(Set(rows.map(\.id)) == [nested, p1])
        review.openSheet(scope: modules)
        let confirmation = try #require(review.sheet).confirmation
        #expect(Set(confirmation.items.map(\.id)) == [nested, p1])
        #expect(confirmation.items.count == rows.count)
        #expect(confirmation.included == 0)
        review.closeSheet()

        // The footer's sheet counts it once, inside its folder.
        review.openSheet()
        #expect(try #require(review.sheet).confirmation.included == 1)
    }

    @Test("the button says how many and from which group, the row how many of how many; nothing selected, or a sheet open, disables it")
    func wordingAndEnabling() throws {
        #expect(Wording.cleanFrom(2, group: "Browser and app caches") == "Clean 2 from Browser and app caches…")
        #expect(Wording.cleanFrom(1_204, group: "Python environments") == "Clean 1,204 from Python environments…")
        #expect(Wording.scopeSummary(selected: 2, of: 3, bytes: 7_730_941_132) == "2 of 3 · 7.2 GiB")
        #expect(Wording.scopeSummary(selected: 0, of: 3, bytes: 0) == "0 of 3")

        let review = try review()
        let items = try #require(review.items)
        let packages = try scope(review, .safe, "Package downloads")
        let selectable = packages.filter { items.row($0)?.selectable == true }.count
        review.set(packages, selected: false)
        #expect(!review.canClean(scope: packages))
        #expect(review.scopeSummary(packages) == "0 of \(selectable)")
        review.toggleAll(packages)
        #expect(review.canClean(scope: packages))
        #expect(review.scopeSummary(packages).hasPrefix("\(selectable) of \(selectable) · "))

        review.openSheet()
        #expect(!review.canClean(scope: packages), "not while a sheet is open")
        #expect(!review.canClean(scope: nil))
    }

    @Test("after a group's clean and Done, the other groups' rows are still selected")
    func keepsTheRest() async throws {
        let report = try Fixture.data("scan-report-groups")
        try data.write(lastScan: report)
        let fake = try FakeCLI(lastScan: data.directory.lastScan)
        try fake.put("dry-run.jsonl", Data("""
            {"type":"item","path":"/Users/example/Library/Caches/Google/Chrome","rule":"app.chrome-cache","class":"safe","action":"delete","bytes":303104,"prediction":"would-delete"}
            {"type":"summary","items":1,"would_delete":1,"would_trash":0,"would_run":0,"would_skip":0,"bytes":303104}

            """.utf8))
        try fake.put("clean-report.json", Data("""
            {"schema_version": 6, "ddc_version": "0.2.1", "platform": "macos", "capabilities": ["private-size", "in-use-check", "dataless-files", "full-disk-access", "local-snapshots", "whole-disk", "coding-agents", "homebrew", "schedule", "notifications"], "cancelled": false, "failed": 0, "local_snapshots": 0,
             "journal": "/Users/example/Library/Logs/DeepDriveCleaner/actions.jsonl", "volumes": [],
             "items": [{"path": "/Users/example/Library/Caches/Google/Chrome", "dev": 16777232, "ino": 340634837,
                        "rule": "app.chrome-cache", "class": "safe", "action": "delete", "bytes": 303104, "outcome": "deleted"}]}
            """.utf8))
        try fake.put("saved-scan.json", report)
        let events = ReviewEvents()
        let snapshot = try ReviewFixture.snapshot(report: report, settingsIn: data)
        let review = makeReview(runner: CLIRunner(executable: fake.executable), data: data, defaults: defaults, events: events) { snapshot }
        _ = review.open(ReviewRequest(group: nil, classes: [.safe, .rebuildable], confirm: false))
        let items = try #require(review.items)
        let chrome = try ReviewFixture.id(items, "Google/Chrome")
        let venv = try ReviewFixture.id(items, "py1/.venv")
        review.selection = [chrome, venv]

        review.openSheet(scope: try scope(review, .safe, "Browser and app caches"))
        try await until("the dry run") { review.sheet?.warnings == .checked }
        review.cleanConfirmedItems()
        try fake.open("clean-start")
        try fake.open("clean-go")
        try await until("the result") { review.sheet?.stage.result != nil }
        try await until("the rescan") { events.busy == [true, false] }
        review.closeSheet()
        #expect(review.isSelected(venv), "the Python environment's tick stays")
        #expect(review.items?.row(chrome)?.lock == .cleaned)
    }
}
