import Foundation
import Testing
@testable import DDCKit

/// Story "User data: move a folder's picked files to the Trash from that
/// folder": a User data folder's own Move button acts on the entries picked
/// there, with no Select all. Temporary directories and defaults suites only.
@Suite("User data: move a folder's picks to the Trash")
@MainActor
struct PickedMoveTests {
    let data: TempDataDirectory
    let defaults: TempDefaults

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
    }

    private func entry(_ group: String) -> ReviewItems.Entry {
        ReviewItems.Entry(riskClass: .userData, group: group)
    }

    private func review(runner: CLIRunner? = nil, events: ReviewEvents = ReviewEvents()) throws -> ReviewModel {
        let snapshot = try ReviewFixture.snapshot(report: UserDataFixture.report(movies: true), settingsIn: data)
        let review = makeReview(runner: runner, data: data, defaults: defaults, events: events) { snapshot }
        _ = review.open(ReviewRequest(group: nil, classes: [], confirm: false))
        return review
    }

    private func id(_ items: ReviewItems, _ suffix: String) throws -> Int {
        try #require(items.rows.last { $0.finding.path.hasSuffix(suffix) }).id
    }

    @Test("Movies has no Select all and a pick scope of its pickable entries; the locked library is not in it")
    func scope() throws {
        let items = try UserDataFixture.items(movies: true)
        let layout = items.layout(entry("Movies"), matching: "")
        #expect(layout.shape == .entries && layout.selectAll == nil)
        #expect(Set(layout.pickScope) == [try id(items, "Holiday.mov"), try id(items, "Movies/Export")])
        #expect(!layout.pickScope.contains(try id(items, ".imovielibrary")))
    }

    @Test("a filter narrows the scope to what it shows, and to nothing when it hides every entry; other shapes have none")
    func filtered() throws {
        let items = try UserDataFixture.items(movies: true)
        #expect(items.layout(entry("Movies"), matching: "holiday").pickScope == [try id(items, "Holiday.mov")])
        #expect(items.layout(entry("Movies"), matching: "nothing like it").pickScope.isEmpty)
        for group in items.sidebar.flatMap(\.entries) {
            let layout = items.layout(group, matching: "")
            if layout.shape != .entries { #expect(layout.pickScope.isEmpty, "\(group.group)") }
            #expect(layout.selectAll == nil || layout.pickScope.isEmpty, "never both rows: \(group.group)")
        }
    }

    @Test("Telegram and WhatsApp list no entries, so they have nothing to move")
    func reportOnly() throws {
        let items = try UserDataFixture.items(movies: true)
        for group in ["Telegram", "WhatsApp"] {
            let layout = items.layout(entry(group), matching: "")
            #expect(layout.pickScope.isEmpty && layout.selectAll == nil, "\(group)")
        }
    }

    @Test("the row and the button say how many and from which folder; with nothing picked, what to do, and the scope cannot be moved")
    func wording() throws {
        let review = try review()
        let items = try #require(review.items)
        let scope = items.layout(entry("Movies"), matching: "").pickScope
        #expect(review.pickedSummary(scope) == "Pick files below to move them to the Trash")
        #expect(Wording.moveFrom(0, group: "Movies") == "Move to the Trash…")
        #expect(!review.canClean(scope: scope))

        review.selection = [try id(items, "Holiday.mov"), try id(items, "Movies/Export")]
        #expect(review.pickedSummary(scope) == "2 picked · \(ByteSize.human(2_900_000_000))")
        #expect(Wording.moveFrom(review.selectedRows(in: scope).count, group: "Movies") == "Move 2 from Movies to the Trash…")
        #expect(review.canClean(scope: scope))
        #expect(Wording.moveFrom(1, group: "iPhone and iPad backups") == "Move 1 from iPhone and iPad backups to the Trash…")
    }

    @Test("the scoped sheet holds the two Movies picks only, both to the Trash, and says Move")
    func sheet() throws {
        let review = try review()
        let items = try #require(review.items)
        let holiday = try id(items, "Holiday.mov")
        let export = try id(items, "Movies/Export")
        let chrome = try ReviewFixture.id(items, "Google/Chrome")
        review.selection = [holiday, export, chrome]

        review.openSheet(scope: items.layout(entry("Movies"), matching: "").pickScope)
        let confirmation = try #require(review.sheet).confirmation
        #expect(Set(confirmation.items.map(\.id)) == [holiday, export])
        #expect(confirmation.items.allSatisfy { $0.effectiveAction == .trash })
        #expect(CleanWording.sheetTitle(confirmation) == "Move 2 items to the Trash?")
        #expect(CleanWording.sheetAction(confirmation) == "Move to the Trash")
        review.closeSheet()
        #expect(review.selection == [holiday, export, chrome])
    }

    @Test("a folder picked in Downloads with a node_modules inside it selected: the scoped sheet holds the folder only")
    func insideAPickedFolder() throws {
        let review = try review()
        let items = try #require(review.items)
        let project = try id(items, "Downloads/Project")
        let nested = try id(items, "Project/node_modules")
        review.selection = [project, nested]

        review.openSheet(scope: items.layout(entry("Downloads"), matching: "").pickScope)
        let confirmation = try #require(review.sheet).confirmation
        #expect(confirmation.items.map(\.id) == [project])
        #expect(confirmation.included == 0, "the sheet says nothing of the node_modules")
    }

    @Test("a mixed sheet says Clean; a Review sheet of Trash items says Move until one is deleted permanently")
    func sheetWording() throws {
        let items = try UserDataFixture.items(movies: true)
        let mixed = Confirmation(items, selection: [try id(items, "big.iso"), try ReviewFixture.id(items, ".npm/_cacache")], deletePermanently: false)
        #expect(CleanWording.sheetTitle(mixed) == "Clean 2 items?")
        #expect(CleanWording.sheetAction(mixed) == "Clean 2 items")
        let one = Confirmation(items, selection: [try id(items, "big.iso")], deletePermanently: false)
        #expect(CleanWording.sheetTitle(one) == "Move 1 item to the Trash?")

        let models = try ReviewFixture.id(items, ".ollama/models")
        var review = Confirmation(items, selection: [models], deletePermanently: false)
        #expect(review.movesOnlyToTrash && CleanWording.sheetAction(review) == "Move to the Trash")
        review.togglePermanent(models)
        #expect(!review.movesOnlyToTrash)
        #expect(CleanWording.sheetTitle(review) == "Clean 1 item?")
    }

    @Test("after a scoped Move and Done, the cache picked elsewhere is still selected")
    func keepsTheRest() async throws {
        let report = try UserDataFixture.report(movies: true)
        try data.write(lastScan: report)
        let fake = try FakeCLI(lastScan: data.directory.lastScan)
        let home = UserDataFixture.home
        try fake.put("dry-run.jsonl", Data("""
            {"type":"item","path":"\(home)/Movies/Holiday.mov","rule":"user.movies","class":"user-data","action":"trash","bytes":2000000000,"prediction":"would-trash"}
            {"type":"summary","items":1,"would_delete":0,"would_trash":1,"would_run":0,"would_skip":0,"bytes":2000000000}

            """.utf8))
        try fake.put("clean-report.json", Data("""
            {"schema_version": 3, "ddc_version": "0.5.0", "cancelled": false, "failed": 0, "local_snapshots": 0,
             "journal": "/Users/example/Library/Logs/DeepDriveCleaner/actions.jsonl", "volumes": [],
             "items": [{"path": "\(home)/Movies/Holiday.mov", "dev": 1, "ino": 301, "rule": "user.movies",
                        "class": "user-data", "action": "trash", "bytes": 2000000000, "outcome": "trashed"}]}
            """.utf8))
        try fake.put("saved-scan.json", report)
        let events = ReviewEvents()
        let review = try review(runner: CLIRunner(executable: fake.executable), events: events)
        let items = try #require(review.items)
        let holiday = try id(items, "Holiday.mov")
        let chrome = try ReviewFixture.id(items, "Google/Chrome")
        review.selection = [holiday, chrome]

        review.openSheet(scope: items.layout(entry("Movies"), matching: "").pickScope)
        try await until("the dry run") { review.sheet?.warnings == .checked }
        review.cleanConfirmedItems()
        try fake.open("clean-start")
        try fake.open("clean-go")
        try await until("the result") { review.sheet?.stage.result != nil }
        try await until("the rescan") { events.busy == [true, false] }
        review.closeSheet()
        #expect(review.isSelected(chrome), "the cache's tick stays")
        #expect(review.items?.row(holiday)?.lock == .cleaned)
    }
}
