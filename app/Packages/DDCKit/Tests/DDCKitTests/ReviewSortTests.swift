import Foundation
import Testing
@testable import DDCKit

/// Story "Review window: sort a group's rows by name, size or last change":
/// one sort for both views, its columns in each direction, the Changed
/// wording, and what sorting leaves alone. Temporary directories and
/// defaults suites only.
@Suite("Review window: sort")
@MainActor
struct ReviewSortTests {
    let data: TempDataDirectory
    let defaults: TempDefaults

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
    }

    static let venvs = ReviewItems.Entry(riskClass: .review, group: "Python environments")
    static let downloads = ReviewItems.Entry(riskClass: .safe, group: "Package downloads")
    /// The fixture's scan time is 2026-10-19; these tests fix their own.
    static let scanned = Date(timeIntervalSince1970: 1_793_354_400)  // 2026-10-30T10:00:00Z
    static let utc = TimeZone(identifier: "UTC")!
    static let english = Locale(identifier: "en_US")
    static let gregorian = Calendar(identifier: .gregorian)

    /// Review venvs in three folders: in `~/temp`, `a2` and `A10` tie on
    /// size and `A10` has no date; in `~/Projects`, `x` and `y` tie on date;
    /// `~/Old` holds one undated venv. Then three Package downloads rules:
    /// npm one row with no date, yarn one row, Homebrew two rows.
    private func report(incomplete: Bool = false) throws -> Data {
        var ino = 9_000
        func venv(_ path: String, _ bytes: Int, _ modified: String?) throws -> [String: Any] {
            ino += 1
            var changes: [String: Any] = [
                "path": "/Users/example/\(path)/.venv", "bytes": bytes, "ino": ino, "class": "review", "rule_class": "rebuildable",
                "removal": "trash", "selected": false, "not_selected": ["reason": "review"], "incomplete": incomplete && path == "temp/b1",
            ]
            if let modified { changes["modified"] = modified }
            return try ReviewFixture.finding(5, changes)
        }
        func cache(_ rule: String, _ path: String, _ bytes: Int, _ modified: String?) throws -> [String: Any] {
            ino += 1
            var changes: [String: Any] = ["rule": rule, "path": "/Users/example/\(path)", "bytes": bytes, "ino": ino]
            if let modified { changes["modified"] = modified }
            return try ReviewFixture.finding(0, changes)
        }
        return try Fixture.editedFindings("scan-report-groups") { findings in
            findings += [
                try! venv("temp/a2", 300_000, "2026-09-01T10:00:00Z"),
                try! venv("temp/A10", 300_000, nil),
                try! venv("temp/b1", 100_000, "2026-10-01T10:00:00Z"),
                try! venv("Projects/x", 500_000, "2025-01-01T00:00:00Z"),
                try! venv("Projects/y", 250_000, "2025-01-01T00:00:00Z"),
                // Over the fixture's minimum size, 64 KiB.
                try! venv("Old/z", 70_000, nil),
                try! cache("cache.yarn", "Library/Caches/Yarn", 812_000, "2026-10-02T00:00:00Z"),
                try! cache("cache.homebrew", "Library/Caches/Homebrew/downloads", 210_000, nil),
                try! cache("cache.homebrew", "Library/Caches/Homebrew/api", 146_000, "2024-03-01T00:00:00Z"),
            ]
        }
    }

    private func review(_ report: Data) throws -> ReviewModel {
        let snapshot = try ReviewFixture.snapshot(report: report, settingsIn: data)
        let review = makeReview(runner: nil, data: data, defaults: defaults) { snapshot }
        _ = review.open(ReviewRequest(group: nil, classes: [.safe, .rebuildable, .review], confirm: false))
        return review
    }

    private static func sort(_ column: ReviewSort.Column, _ ascending: Bool) -> ReviewSort {
        ReviewSort(column: column, ascending: ascending)
    }

    /// Each subgroup's title and its rows as shown.
    private func subgroups(_ layout: GroupLayout) -> [String: [String]] {
        var out: [String: [String]] = [:]
        for block in layout.blocks {
            if case .subgroup(let subgroup) = block { out[subgroup.title] = subgroup.items.map(\.shown) }
        }
        return out
    }

    private func titles(_ layout: GroupLayout) -> [String] {
        layout.blocks.map { block in
            switch block {
            case .subgroup(let subgroup): subgroup.title
            case .single(let single): single.title
            case .entries: ""
            }
        }
    }

    // MARK: - The report

    @Test("a report without modified still decodes; null reads as no date; a value and an entry's own date are kept")
    func decodes() throws {
        let plain = try Fixture.groupsReport()
        #expect(plain.findings.allSatisfy { $0.modified == nil })

        let dated = try ScanReport.read(Fixture.editedFindings("scan-report-groups") { findings in
            findings[0]["modified"] = NSNull()
            findings[2]["modified"] = "2026-09-25T14:03:00Z"
        })
        #expect(dated.findings[0].modified == nil)
        #expect(dated.findings[2].modified == "2026-09-25T14:03:00Z")

        // A picked entry's synthesized finding carries the entry's own change.
        let items = try UserDataFixture.items()
        let iso = try #require(items.rows.last { $0.finding.path.hasSuffix("big.iso") })
        #expect(iso.finding.modified == "2033-05-08T03:33:20Z")
        #expect(items.rows.last { $0.finding.path.hasSuffix("Downloads/Project") }?.finding.modified == nil)
    }

    // MARK: - The columns

    @Test("each column in each direction orders the subgroups and their rows; undated last both ways; ties by size, then id or tag")
    func groupsTable() throws {
        let items = try ReviewFixture.items(report: report())
        let temp = ["a2/.venv", "A10/.venv", "b1/.venv"]
        let table: [(ReviewSort, [String], [String], [String])] = [
            (Self.sort(.size, false), ["~/Projects", "~/temp", "~/Old"], ["a2/.venv", "A10/.venv", "b1/.venv"], ["x/.venv", "y/.venv"]),
            (Self.sort(.size, true), ["~/Old", "~/temp", "~/Projects"], ["b1/.venv", "a2/.venv", "A10/.venv"], ["y/.venv", "x/.venv"]),
            // Finder's order: case-insensitive, numbers by value.
            (Self.sort(.name, true), ["~/Old", "~/Projects", "~/temp"], ["a2/.venv", "A10/.venv", "b1/.venv"], ["x/.venv", "y/.venv"]),
            (Self.sort(.name, false), ["~/temp", "~/Projects", "~/Old"], ["b1/.venv", "A10/.venv", "a2/.venv"], ["y/.venv", "x/.venv"]),
            // A subgroup by its newest date; the one with none last both ways;
            // x and y tie on the date, so size decides in both directions.
            (Self.sort(.changed, true), ["~/Projects", "~/temp", "~/Old"], ["a2/.venv", "b1/.venv", "A10/.venv"], ["x/.venv", "y/.venv"]),
            (Self.sort(.changed, false), ["~/temp", "~/Projects", "~/Old"], ["b1/.venv", "a2/.venv", "A10/.venv"], ["x/.venv", "y/.venv"]),
        ]
        for (sort, order, tempRows, projectRows) in table {
            let layout = items.layout(Self.venvs, matching: "", sort: sort)
            #expect(titles(layout) == order, "\(sort)")
            #expect(subgroups(layout)["~/temp"] == tempRows, "\(sort)")
            #expect(subgroups(layout)["~/Projects"] == projectRows, "\(sort)")
            #expect(Set(subgroups(layout)["~/temp"] ?? []) == Set(temp))
        }

        // The subgroup's Changed column: its newest row.
        let layout = items.layout(Self.venvs, matching: "")
        let headings = layout.blocks.compactMap { if case .subgroup(let subgroup) = $0 { subgroup } else { nil } }
        #expect(headings.map(\.modified) == ["2025-01-01T00:00:00Z", "2026-10-01T10:00:00Z", nil])

        // A one-row rule sorts with the subgroups, by its title, size and date.
        let rules: [(ReviewSort, [String])] = [
            (Self.sort(.size, false), ["npm", "yarn", "homebrew"]),
            (Self.sort(.name, true), ["homebrew", "npm", "yarn"]),
            (Self.sort(.changed, true), ["homebrew", "yarn", "npm"]),
            (Self.sort(.changed, false), ["yarn", "homebrew", "npm"]),
        ]
        for (sort, order) in rules {
            #expect(titles(items.layout(Self.downloads, matching: "", sort: sort)) == order, "\(sort)")
        }
    }

    @Test("the Items view orders by the path under ~, size or change, after the filter")
    func itemsTable() throws {
        let items = try ReviewFixture.items(report: report())
        let mine = ["temp/a2", "temp/A10", "temp/b1", "Projects/x", "Projects/y", "Old/z"]
        func order(_ sort: ReviewSort, _ query: String = "") -> [String] {
            items.items(matching: query, sort: sort).compactMap { row in
                mine.first { row.finding.path == "/Users/example/\($0)/.venv" }
            }
        }
        #expect(order(Self.sort(.size, false)) == ["Projects/x", "temp/a2", "temp/A10", "Projects/y", "temp/b1", "Old/z"])
        #expect(order(Self.sort(.size, true)) == ["Old/z", "temp/b1", "Projects/y", "temp/a2", "temp/A10", "Projects/x"])
        #expect(order(Self.sort(.name, true)) == ["Old/z", "Projects/x", "Projects/y", "temp/a2", "temp/A10", "temp/b1"])
        #expect(order(Self.sort(.name, false)) == ["temp/b1", "temp/A10", "temp/a2", "Projects/y", "Projects/x", "Old/z"])
        // Undated last both ways, the two of them largest first.
        #expect(order(Self.sort(.changed, true)) == ["Projects/x", "Projects/y", "temp/a2", "temp/b1", "temp/A10", "Old/z"])
        #expect(order(Self.sort(.changed, false)) == ["temp/b1", "temp/a2", "Projects/x", "Projects/y", "temp/A10", "Old/z"])

        // The filter first: only what it lets through is sorted.
        #expect(order(Self.sort(.changed, true), "1") == ["temp/b1", "temp/A10"])
        let filtered = items.layout(Self.venvs, matching: "1", sort: Self.sort(.changed, true))
        #expect(titles(filtered) == ["~/temp"])
        #expect(subgroups(filtered)["~/temp"] == ["b1/.venv", "A10/.venv"])
    }

    @Test("the comparator: a time after the scan sorts as the newest; equal keys leave it to the caller")
    func comparator() {
        let late = ReviewSort.Key(name: "a", bytes: 1, changed: "2099-01-01T00:00:00Z")
        let early = ReviewSort.Key(name: "b", bytes: 1, changed: "2026-01-01T00:00:00Z")
        let none = ReviewSort.Key(name: "c", bytes: 9, changed: nil)
        #expect(Self.sort(.changed, false).precedes(late, early) == true)
        #expect(Self.sort(.changed, true).precedes(early, late) == true)
        #expect(Self.sort(.changed, true).precedes(none, early) == false)
        #expect(Self.sort(.changed, false).precedes(none, early) == false)
        #expect(Self.sort(.changed, false).precedes(early, none) == true)
        #expect(Self.sort(.size, false).precedes(late, early) == nil)
        #expect(Self.sort(.name, true).precedes(ReviewSort.Key(name: "File10", bytes: 1, changed: nil), ReviewSort.Key(name: "file9", bytes: 1, changed: nil)) == false)
    }

    // MARK: - The default and the stored sort

    @Test("the default is Size, largest first, which gives today's order in both views; the old orders are Size descending and Name ascending")
    func defaultOrder() throws {
        #expect(ReviewSort.default == Self.sort(.size, false))
        let review = try review(report())
        #expect(review.sort == .default)

        let items = try ReviewFixture.items(report: report())
        // The Items view: Biggest First was bytes, largest first, then id.
        let shown = items.rows.filter { $0.lock != .userData && $0.userEntry == nil }
        let biggest = shown.sorted { $0.finding.bytes != $1.finding.bytes ? $0.finding.bytes > $1.finding.bytes : $0.id < $1.id }
        #expect(items.items(matching: "", sort: .default).map(\.id) == biggest.map(\.id))
        // By Path was the path's code points: the same on the plain fixture,
        // where no name differs by case or by a number's width.
        let plain = try ReviewFixture.items()
        let byPath = plain.rows.filter { $0.lock != .userData && $0.userEntry == nil }
            .sorted { $0.finding.path != $1.finding.path ? $0.finding.path < $1.finding.path : $0.id < $1.id }
        #expect(plain.items(matching: "", sort: Self.sort(.name, true)).map(\.id) == byPath.map(\.id))
        // Where they do, Name is Finder's order: pip before Yarn, Old before p1.
        let names = items.items(matching: "", sort: Self.sort(.name, true)).map { Wording.tilde($0.finding.path, home: "/Users/example") }
        let index = { (path: String) in names.firstIndex(of: path) ?? -1 }
        #expect(index("~/Library/Caches/pip") < index("~/Library/Caches/Yarn"))
        #expect(index("~/Old/z/.venv") < index("~/p1/node_modules"))
        #expect(index("~/p2/node_modules") < index("~/Projects/x/.venv"))

        // GroupLayout: blocks by bytes, then tag; rows by bytes, then id.
        for entry in items.sidebar.flatMap(\.entries) {
            let layout = items.layout(entry, matching: "")
            #expect(layout == items.layout(entry, matching: "", sort: .default))
            let bytes = layout.blocks.map(\.bytes)
            #expect(bytes == bytes.sorted(by: >), "\(entry.group)")
            for case .subgroup(let subgroup) in layout.blocks {
                let today = subgroup.items.sorted { $0.row.finding.bytes != $1.row.finding.bytes ? $0.row.finding.bytes > $1.row.finding.bytes : $0.id < $1.id }
                #expect(subgroup.items.map(\.id) == today.map(\.id))
            }
        }
    }

    @Test("a column title sorts by it, again reverses it; the sort is kept across groups and launches; an unreadable one reads as the default")
    func stored() throws {
        let review = try review(report())
        review.sortBy(.changed)
        #expect(review.sort == Self.sort(.changed, true), "Changed starts oldest first")
        review.sortBy(.changed)
        #expect(review.sort == Self.sort(.changed, false))
        review.sortBy(.name)
        #expect(review.sort == Self.sort(.name, true))
        review.sortBy(.size)
        #expect(review.sort == Self.sort(.size, false), "Size starts largest first")
        review.sortBy(.changed)

        review.entry = Self.downloads
        review.reload()
        #expect(review.sort == Self.sort(.changed, true), "across groups")
        #expect(AppPreferences(defaults: defaults.defaults).reviewSort == Self.sort(.changed, true))
        let relaunched = makeReview(runner: nil, data: data, defaults: defaults) { nil }
        #expect(relaunched.sort == Self.sort(.changed, true), "across launches")

        let preferences = AppPreferences(defaults: defaults.defaults)
        for unreadable: Any in [Data("garbage".utf8), "changed", 3, Data(#"{"column":"idle","ascending":true}"#.utf8), Data(#"{"column":"size"}"#.utf8)] {
            defaults.defaults.set(unreadable, forKey: AppPreferences.reviewSortKey)
            #expect(preferences.reviewSort == .default, "\(unreadable)")
        }
        defaults.defaults.removeObject(forKey: AppPreferences.reviewSortKey)
        #expect(preferences.reviewSort == .default)
        #expect(makeReview(runner: nil, data: data, defaults: defaults) { nil }.sort == .default)
    }

    // MARK: - What sorting leaves alone

    @Test("sorting leaves the selection, the Select all state and a group's clean scope unchanged")
    func selectionUnchanged() throws {
        let review = try review(report())
        let items = try #require(review.items)
        let scope = try #require(items.layout(Self.venvs, matching: "").selectAll)
        review.set(scope, selected: false)
        review.toggle(try ReviewFixture.id(items, "temp/A10/.venv"))
        review.toggle(try ReviewFixture.id(items, "Projects/y/.venv"))
        let selection = review.selection
        let state = review.checkState(scope)
        let picked = review.selectedRows(in: scope).map(\.id)
        #expect(state == .mixed)

        for column in ReviewSort.Column.allCases {
            for _ in 0..<2 {
                review.sortBy(column)
                let layout = items.layout(Self.venvs, matching: "", sort: review.sort)
                let all = try #require(layout.selectAll)
                #expect(Set(all) == Set(scope), "\(review.sort)")
                #expect(review.selection == selection)
                #expect(review.checkState(all) == state)
                #expect(review.selectedRows(in: all).map(\.id) == picked)
                #expect(review.scopeSummary(all) == review.scopeSummary(scope))
            }
        }
        review.openSheet(scope: items.layout(Self.venvs, matching: "", sort: review.sort).selectAll)
        #expect(Set(try #require(review.sheet).confirmation.items.map(\.id)) == Set(picked))

        // A user-data folder's pick scope is the same set in every order.
        let user = try UserDataFixture.items()
        let downloads = ReviewItems.Entry(riskClass: .userData, group: "Downloads")
        let pick = Set(user.layout(downloads, matching: "").pickScope)
        #expect(!pick.isEmpty)
        for column in ReviewSort.Column.allCases {
            for ascending in [true, false] {
                #expect(Set(user.layout(downloads, matching: "", sort: Self.sort(column, ascending)).pickScope) == pick)
            }
        }
    }

    // MARK: - The words

    @Test("the Changed column against the scan's time: Today, Yesterday, an abbreviated date, a dash with no date; the time zone counts")
    func changedWording() {
        func changed(_ modified: String?, scanned: Date? = Self.scanned, zone: TimeZone = Self.utc) -> String {
            CleanWording.changed(modified, scanned: scanned, calendar: Self.gregorian, locale: Self.english, timeZone: zone)
        }
        #expect(changed("2026-10-30T01:00:00Z") == "Today")
        #expect(changed("2026-10-29T23:59:59Z") == "Yesterday")
        #expect(changed("2026-10-28T12:00:00Z") == "Oct 28, 2026")
        #expect(changed("2026-09-25T14:03:00Z") == "Sep 25, 2026")
        #expect(changed(nil) == "—")
        #expect(changed("not a date") == "—")
        // A wrong clock: shown as it is.
        #expect(changed("2026-11-02T00:00:00Z") == "Nov 2, 2026")
        // Days are the user's: 20:00 UTC on the 29th is the 30th at UTC+5.
        #expect(changed("2026-10-29T20:00:00Z") == "Yesterday")
        #expect(changed("2026-10-29T20:00:00Z", zone: TimeZone(secondsFromGMT: 5 * 3600)!) == "Today")
        // Without the scan's time, a date.
        #expect(changed("2026-10-30T01:00:00Z", scanned: nil) == "Oct 30, 2026")
    }

    @Test("the tooltips: a finding's newest change inside, 'at least' when incomplete; an entry's own change; the second line says Project untouched and no Last changed")
    func tooltips() throws {
        func help(_ row: ReviewItems.Row) -> String? {
            CleanWording.changedHelp(row, calendar: Self.gregorian, locale: Self.english, timeZone: Self.utc)
        }
        let items = try ReviewFixture.items(report: report(incomplete: true))
        let a2 = try #require(items.row(try ReviewFixture.id(items, "temp/a2/.venv")))
        let a2Help = try #require(help(a2))
        #expect(a2Help.hasPrefix("Newest change inside: Sep 1, 2026"), "\(a2Help)")
        #expect(a2Help.contains("10:00"))
        #expect(!a2Help.contains("at least"))
        let b1 = try #require(items.row(try ReviewFixture.id(items, "temp/b1/.venv")))
        let b1Help = try #require(help(b1))
        #expect(b1Help.hasPrefix("Newest change inside: at least") && b1Help.contains("Oct 1, 2026"), "\(b1Help)")
        #expect(help(try #require(items.row(try ReviewFixture.id(items, "temp/A10/.venv")))) == nil)
        let headings = items.layout(Self.venvs, matching: "").blocks.compactMap { if case .subgroup(let subgroup) = $0 { subgroup } else { nil } }
        let projects = try #require(headings.first { $0.title == "~/Projects" })
        let heading = try #require(CleanWording.subgroupChangedHelp(projects.modified, calendar: Self.gregorian, locale: Self.english, timeZone: Self.utc))
        #expect(heading.hasPrefix("Newest change among them: Jan 1, 2025"), "\(heading)")

        let user = try UserDataFixture.items()
        let iso = try #require(user.rows.last { $0.finding.path.hasSuffix("big.iso") })
        let isoHelp = try #require(help(iso))
        #expect(isoHelp.hasPrefix("Changed: May 8, 2033"), "\(isoHelp)")
        let entries = user.layout(ReviewItems.Entry(riskClass: .userData, group: "Downloads"), matching: "").blocks.flatMap { block -> [GroupLayout.Item] in
            if case .entries(let items) = block { items } else { [] }
        }
        #expect(!entries.isEmpty && entries.allSatisfy { !$0.detail.contains("Last changed") })

        // The project's activity keeps its own name beside the column.
        let details = headings.flatMap(\.items).map(\.detail)
        let detail = try #require(details.first)
        #expect(detail.hasPrefix("Project untouched 656 days"), "\(detail)")
        #expect(CleanWording.projectUntouched(1) == "Project untouched 1 day")
    }

    @Test("the header's labels and the Items heading name the sort")
    func headings() {
        let table: [(ReviewSort, String, String)] = [
            (Self.sort(.size, false), "Items · largest first", "Size, sorted largest first"),
            (Self.sort(.size, true), "Items · smallest first", "Size, sorted smallest first"),
            (Self.sort(.name, true), "Items · by name, A to Z", "Name, sorted A to Z"),
            (Self.sort(.name, false), "Items · by name, Z to A", "Name, sorted Z to A"),
            (Self.sort(.changed, true), "Items · oldest change first", "Changed, sorted oldest first"),
            (Self.sort(.changed, false), "Items · newest change first", "Changed, sorted newest first"),
        ]
        for (sort, heading, label) in table {
            #expect(CleanWording.itemsHeading(sort) == heading)
            #expect(CleanWording.sortLabel(sort.column, sort: sort) == label)
            #expect(CleanWording.sortHint(sort.column, sort: sort) == "Reverses the order")
            for other in ReviewSort.Column.allCases where other != sort.column {
                #expect(CleanWording.sortLabel(other, sort: sort) == CleanWording.sortTitle(other))
            }
        }
        #expect(ReviewSort.Column.allCases.map(CleanWording.sortTitle) == ["Name", "Size", "Changed"])
    }
}
