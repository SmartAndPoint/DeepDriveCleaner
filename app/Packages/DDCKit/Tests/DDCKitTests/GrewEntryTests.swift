import Foundation
import Testing
@testable import DDCKit

/// The Review window's "Grew since …" entry (story "Popover and Review: what
/// grew since the earlier scan", DDC-002), over the CLI's fixtures: the
/// groups report, and what `compare` wrote for it against an earlier one
/// without `r1/target` and with a smaller `p1/node_modules`.
@Suite("Review: grew since the earlier scan", .timeLimit(.minutes(1)))
@MainActor
struct GrewEntryTests {
    let data: TempDataDirectory
    let defaults: TempDefaults

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
    }

    static let now = Date(timeIntervalSince1970: 1_792_404_000)  // 2026-10-19T10:00:00Z
    static let G: Int64 = 1 << 30

    private func items() throws -> ReviewItems {
        ReviewItems.make(report: try ScanReport.read(Fixture.data("scan-report-groups")), settings: nil, rules: try Fixture.rules())
    }

    private func changes(_ edit: (inout [String: Any]) -> Void = { _ in }) throws -> ChangesReport {
        try ChangesReport.read(Fixture.edited("changes-compared", edit))
    }

    private func entry(_ changes: ChangesReport, sort: GrewEntry.Sort = .default) throws -> GrewEntry? {
        GrewEntry.make(items: try items(), changes: changes, sort: sort, home: "/Users/example", now: Self.now)
    }

    /// A changed row of the result at `path`.
    private static func item(_ path: String, rule: String, kind: String, delta: Int64) -> [String: Any] {
        ["path": path, "rule": rule, "kind": kind, "before": delta < 0 ? -delta : NSNull(), "now": delta > 0 ? delta : NSNull(), "delta": delta, "cleaned": 0, "ignored": false] as [String: Any]
    }

    // MARK: - The value

    @Test("the rows that grew are the window's own, grouped by their Review group, biggest change first")
    func rows() throws {
        let items = try items()
        let grew = try #require(try entry(try changes()))
        let target = try ReviewFixture.id(items, "r1/target")
        let modules = try ReviewFixture.id(items, "p1/node_modules")
        #expect(grew.groups.map { $0.rows.map(\.id) } == [[target], [modules]])
        #expect(grew.groups.map(\.entry.group) == [try #require(items.row(target)).group, try #require(items.row(modules)).group])
        #expect(grew.groups.flatMap(\.rows).map(\.change) == ["new", "+195.3 KiB"])
        #expect(grew.title.hasPrefix("Grew since yesterday "))
        #expect(grew.amount == "+539.3 KiB", "home.delta")
        #expect(grew.summary.hasPrefix("60.0 GiB more in use on the startup disk: 539.3 KiB in your home folder, 60.0 GiB in folders ddc did not read"))
        #expect(grew.rest == "+60.0 GiB" && grew.elsewhere == nil)
        #expect(grew.notRead.isEmpty && grew.gone.isEmpty && grew.cleanedSince == nil)
    }

    @Test("Name and Size order the rows too; a column clicked twice reverses")
    func sort() throws {
        var sort = GrewEntry.Sort.default
        #expect(sort.column == .change && !sort.ascending)
        sort = sort.clicking(.name)
        #expect(sort == GrewEntry.Sort(column: .name, ascending: true))
        #expect(sort.clicking(.name) == GrewEntry.Sort(column: .name, ascending: false))
        // Two rows in one group: Downloads and Pictures are user data, each its own group;
        // two node_modules folders share theirs.
        let both = try changes { json in
            json["items"] = [
                Self.item("/Users/example/p1/node_modules", rule: "node.node_modules", kind: "grew", delta: 100),
                Self.item("/Users/example/p2/node_modules", rule: "node.node_modules", kind: "grew", delta: 900),
            ]
        }
        let byChange = try #require(try entry(both))
        let paths = { (grew: GrewEntry) throws -> [String] in
            let items = try self.items()
            return try grew.groups[0].rows.map { try #require(items.row($0.id)).finding.path }
        }
        #expect(try paths(byChange) == ["/Users/example/p2/node_modules", "/Users/example/p1/node_modules"])
        let byName = try #require(try entry(both, sort: GrewEntry.Sort(column: .name, ascending: true)))
        #expect(try paths(byName) == ["/Users/example/p1/node_modules", "/Users/example/p2/node_modules"])
    }

    @Test("user data keeps its own group and no Select all; a rebuildable group has it, over the rows shown only")
    func groupsAndSelectAll() throws {
        let items = try items()
        let grew = try #require(try entry(try changes { json in
            var rows = json["items"] as! [[String: Any]]
            rows.append(Self.item("/Users/example/Downloads", rule: "user.downloads", kind: "grew", delta: 3 * Self.G))
            json["items"] = rows
        }))
        let downloads = try #require(grew.groups.first { $0.entry.riskClass == .userData })
        #expect(downloads.entry.group == (try #require(items.row(try ReviewFixture.id(items, "/Downloads")))).group)
        #expect(downloads.selectAll == nil)
        let modules = try #require(grew.groups.first { $0.rows.contains { $0.id == (try? ReviewFixture.id(items, "p1/node_modules")) } })
        let ids = try #require(modules.selectAll, "node_modules has Select all in its own entry")
        #expect(ids == modules.rows.map(\.id), "only the rows the entry shows")
        #expect(try #require(items.layout(modules.entry, matching: "").selectAll).count > ids.count, "not the whole group")
        #expect(grew.groups.first?.entry.riskClass == .userData, "3 GiB: the biggest change first")
    }

    @Test("no entry when not compared, or when nothing matched grew and the CLI does not say grew; with grew alone, the sections only")
    func shown() throws {
        #expect(try entry(try ChangesReport.read(Fixture.data("changes-not-compared"))) == nil)
        let none = try changes { json in
            json["items"] = [Self.item("/Users/example/elsewhere/target", rule: "rust.target", kind: "new", delta: 5)]
            json["grew"] = false
        }
        #expect(try entry(none) == nil)
        let alone = try #require(try entry(try changes { $0["items"] = [] }))
        #expect(alone.groups.isEmpty && alone.rest == "+60.0 GiB")
        // An ignored row is never matched.
        let ignored = try changes { json in
            var rows = json["items"] as! [[String: Any]]
            for index in rows.indices { rows[index]["ignored"] = true }
            json["items"] = rows
            json["grew"] = false
        }
        #expect(try entry(ignored) == nil)
    }

    @Test("a match is by path bytes and rule: another rule, another spelling of the name, a moved row with shared bytes")
    func matching() throws {
        let items = try items()
        let modules = try ReviewFixture.id(items, "p1/node_modules")
        var moved = Self.item("/Users/example/p1/node_modules", rule: "node.node_modules", kind: "moved", delta: 4096)
        moved["approximate"] = true
        let grew = try #require(try entry(try changes { json in
            json["items"] = [
                moved,
                Self.item("/Users/example/p2/node_modules", rule: "rust.target", kind: "new", delta: 7),
                // "p2" written with a combining character: another name in bytes.
                Self.item("/Users/example/p2\u{0301}/node_modules", rule: "node.node_modules", kind: "new", delta: 9),
            ]
        }))
        #expect(grew.groups.flatMap(\.rows).map(\.id) == [modules])
        #expect(grew.groups.flatMap(\.rows).map(\.change) == ["≈ moved"])
    }

    @Test("a type switched off, a row under the minimum size, and a user-data entry are never matched")
    func filters() throws {
        let settings = try SharedSettings.read(at: {
            try data.write(settings: #"{"schema_version": 1, "min_size": 500000, "disabled_rules": ["node.node_modules"]}"#)
            return data.directory.settings
        }())
        let items = ReviewItems.make(report: try ScanReport.read(Fixture.data("scan-report-groups")), settings: settings, rules: try Fixture.rules())
        #expect(items.row(try ReviewFixture.id(items, "r1/target"))?.lock == .underMinimumSize)
        #expect(items.row(try ReviewFixture.id(items, "p1/node_modules"))?.lock == .typeOff)
        let grew = try #require(GrewEntry.make(items: items, changes: try changes(), home: "/Users/example", now: Self.now))
        #expect(grew.groups.isEmpty, "both changed rows are ignored now; the CLI's grew keeps the sections")
        // A picked entry of a user-data folder carries its folder's rule at another path.
        let origin = ReviewItems.make(report: try ScanReport.read(Fixture.data("scan-report-origin")), settings: nil, rules: try Fixture.rules())
        let picked = try #require(origin.rows.first { $0.userEntry != nil })
        let entryOnly = GrewEntry.make(items: origin, changes: try changes { json in
            json["items"] = [Self.item(picked.finding.path, rule: picked.finding.rule, kind: "new", delta: 5)]
            json["grew"] = false
        }, home: "/Users/example", now: Self.now)
        #expect(entryOnly == nil)
    }

    @Test("the filter keeps the rows it matches, and Select all over them only")
    func filter() throws {
        let items = try items()
        let both = try #require(try entry(try changes { json in
            json["items"] = [
                Self.item("/Users/example/p1/node_modules", rule: "node.node_modules", kind: "grew", delta: 100),
                Self.item("/Users/example/p2/node_modules", rule: "node.node_modules", kind: "grew", delta: 900),
            ]
        }))
        let shown = both.filtered(items: items, query: "p1/")
        #expect(shown.groups.flatMap(\.rows).map(\.id) == [try ReviewFixture.id(items, "p1/node_modules")])
        #expect(shown.groups.first?.selectAll.map(Set.init) ?? [] == Set(shown.groups.flatMap(\.rows).map(\.id)))
        #expect(both.filtered(items: items, query: "nothing-like-this").groups.isEmpty)
        #expect(both.filtered(items: items, query: " ") == both)
    }

    @Test("the sidebar number: the home's growth, else the groups' in findings, else none")
    func amount() throws {
        let notCompared = try changes { json in
            json["home"] = ["compared": false, "reasons": ["different-access"], "delta": NSNull(), "ignored_delta": 0, "elsewhere": NSNull()]
        }
        let grew = try #require(try entry(notCompared))
        #expect(grew.amount == "+539.3 KiB in findings")
        #expect(grew.summary == "Home total not compared: one scan had Full Disk Access and the other not.")
        let shrank = try changes { json in
            var home = json["home"] as! [String: Any]
            home["delta"] = -5
            json["home"] = home
            json["groups"] = []
        }
        #expect(try #require(try entry(shrank)).amount == nil)
    }

    @Test("what was not read, by kind, with Open Privacy Settings only for a scan without Full Disk Access; the person's cleans")
    func notReadAndCleans() throws {
        let grew = try #require(try entry(try changes { json in
            var rest = json["rest"] as! [String: Any]
            rest["not_read"] = [
                ["path": "/Users/example/Library/Mail", "kind": "prompting"],
                ["path": "/Users/example/Desktop", "kind": "prompting"],
                ["path": "/Users/example/Library/CloudStorage/x", "kind": "cloud"],
            ]
            json["rest"] = rest
            json["cleaned_since"] = ["deleted": 3 * Self.G, "trashed": Self.G]
            var rows = json["items"] as! [[String: Any]]
            var cleaned = Self.item("/Users/example/old/target", rule: "rust.target", kind: "cleaned", delta: -3 * Self.G)
            cleaned["cleaned"] = 3 * Self.G
            rows.append(cleaned)
            json["items"] = rows
        }))
        #expect(grew.notRead.map(\.kind) == [.cloud, .prompting])
        #expect(grew.notRead.map(\.count) == [1, 2])
        #expect(grew.offersPrivacy, "the fixture's scan had no Full Disk Access")
        #expect(grew.cleanedNote != nil)
        #expect(grew.cleanedSince == "You cleaned 4.0 GiB since then; 1.0 GiB of it is in the Trash.")
        #expect(grew.gone.map(\.text) == ["cleaned 3.0 GiB"])
        #expect(grew.gone.map(\.path) == ["~/old/target"])
    }

    @Test("a project scan against a home baseline says where it compared")
    func roots() throws {
        let grew = try #require(try entry(try changes { $0["compared_roots"] = ["/Users/example/p1"] }))
        #expect(grew.rootsNote == "Compared in ~/p1 only: the earlier scan covered other folders.")
    }

    // MARK: - The window

    final class Shown {
        var snapshot: ReviewSnapshot?
        init(_ snapshot: ReviewSnapshot?) { self.snapshot = snapshot }
    }

    private func snapshot(_ changes: ChangesReport?) throws -> ReviewSnapshot {
        ReviewSnapshot(report: try ScanReport.read(Fixture.data("scan-report-groups")), rules: try Fixture.rules(), changes: changes)
    }

    private static let request = ReviewRequest(group: nil, classes: [.safe, .rebuildable], confirm: false, keepsSelection: true, grew: .top)

    @Test("the link opens the entry; with the window open the person's ticks stay, and a tick there is the group's tick")
    func window() throws {
        let shown = Shown(try snapshot(try changes()))
        let events = ReviewEvents()
        let review = makeReview(runner: nil, data: data, defaults: defaults, events: events, clock: { Self.now }) { shown.snapshot }
        // A Review… first: the window on a group, Items mode, a tick.
        #expect(review.open(ReviewRequest(group: "node_modules", classes: [.safe, .rebuildable], confirm: false)))
        let items = try #require(review.items)
        let modules = try ReviewFixture.id(items, "p1/node_modules")
        review.toggle(modules)
        let ticked = review.selection
        review.mode = .items
        #expect(review.open(Self.request))
        #expect(review.grewSince && review.mode == .groups && !review.wholeDisk && review.grewSection == .top)
        #expect(review.selection == ticked)
        let grew = try #require(review.grew)
        #expect(grew.groups.flatMap(\.rows).contains { $0.id == modules })
        review.toggle(modules)
        #expect(review.selection != ticked, "the same row, so the same tick")
        // Leaving for a group or Whole disk.
        review.leaveGrew()
        #expect(!review.grewSince)
        review.showGrew(at: .notRead)
        #expect(review.grewSince && review.grewSection == .notRead)
        // A request without a group from elsewhere starts afresh.
        #expect(review.open(ReviewRequest(group: "node_modules", classes: [.safe], confirm: false)))
        #expect(!review.grewSince)
    }

    @Test("Scan Again and the reload after a clean rebuild the entry from the new result, and leave it when nothing grew")
    func rebuilds() async throws {
        let shown = Shown(try snapshot(try changes()))
        let events = ReviewEvents()
        let review = makeReview(runner: nil, data: data, defaults: defaults, events: events, clock: { Self.now }) { shown.snapshot }
        #expect(review.open(Self.request))
        #expect(review.grewSince)
        // The scan finds p1/node_modules grown more.
        shown.snapshot = try snapshot(try changes { json in
            var rows = json["items"] as! [[String: Any]]
            rows[1]["delta"] = 2 * Self.G
            json["items"] = rows
        })
        await review.scanAgain()
        #expect(events.rescans == 1)
        #expect(review.grewSince)
        #expect(review.grew?.groups.flatMap(\.rows).map(\.change).contains("+2.0 GiB") == true)
        // A clean of p1/node_modules: the next result no longer has it, other rows still grew.
        shown.snapshot = try snapshot(try changes { json in
            var rows = json["items"] as! [[String: Any]]
            rows.removeLast()
            json["items"] = rows
        })
        review.reload(afterClean: true)
        #expect(review.grewSince, "the window stays on the entry")
        let items = try #require(review.items)
        #expect(review.grew?.groups.flatMap(\.rows).map(\.id) == [try ReviewFixture.id(items, "r1/target")])
        // A result that compares nothing: the entry goes.
        shown.snapshot = try snapshot(nil)
        review.reload()
        #expect(review.grew == nil && !review.grewSince)
    }
}
