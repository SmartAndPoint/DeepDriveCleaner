import Foundation
import Testing
@testable import DDCKit

/// Story "Review window: sorted sidebar, Select all and folder groups": the
/// sidebar's order, the subgroups, Select all and the rows' own facts.
/// Temporary directories and defaults suites only.
@Suite("Review window: groups")
@MainActor
struct ReviewGroupsTests {
    let data: TempDataDirectory
    let defaults: TempDefaults

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
    }

    private func groups(_ items: ReviewItems, _ riskClass: ScanReport.RiskClass) -> [String] {
        items.sidebar.first { $0.riskClass == riskClass }?.entries.map(\.group) ?? []
    }

    private func subgroups(_ layout: GroupLayout) -> [GroupLayout.Subgroup] {
        layout.blocks.compactMap { if case .subgroup(let subgroup) = $0 { subgroup } else { nil } }
    }

    private func singles(_ layout: GroupLayout) -> [GroupLayout.Single] {
        layout.blocks.compactMap { if case .single(let single) = $0 { single } else { nil } }
    }

    private func review(_ report: Data) throws -> ReviewModel {
        let snapshot = try ReviewFixture.snapshot(report: report, settingsIn: data)
        let review = makeReview(runner: nil, data: data, defaults: defaults) { snapshot }
        _ = review.open(ReviewRequest(group: nil, classes: [.safe, .rebuildable], confirm: false))
        return review
    }

    @Test("the sidebar orders each class's groups by total bytes, largest first; ties keep the rules' order; ticking moves nothing")
    func sidebarOrder() throws {
        // `rules --json` lists Python environments, node_modules, Build outputs.
        let items = try ReviewFixture.items()
        #expect(groups(items, .rebuildable) == ["node_modules", "Python environments", "Build outputs"])
        #expect(groups(items, .safe) == ["Package caches", "App caches"])

        let chrome = try Fixture.editedFindings("scan-report-groups") { $0[2]["bytes"] = 2_000_000 }
        #expect(groups(try ReviewFixture.items(report: chrome), .safe) == ["App caches", "Package caches"])
        // pip's type is switched off: its size counts for nothing.
        let pip = try Fixture.editedFindings("scan-report-groups") { $0[1]["bytes"] = 10_000_000 }
        #expect(groups(try ReviewFixture.items(report: pip), .safe) == ["Package caches", "App caches"])
        // Equal totals keep the rules' order, either way round.
        let tied = try Fixture.editedFindings("scan-report-groups") { $0[6]["bytes"] = 802_816 }
        #expect(groups(try ReviewFixture.items(report: tied), .rebuildable) == ["node_modules", "Build outputs", "Python environments"])
        let first = try Fixture.editedFindings("scan-report-groups") { $0[5]["bytes"] = 802_816 }
        #expect(groups(try ReviewFixture.items(report: first), .rebuildable) == ["Python environments", "node_modules", "Build outputs"])

        let review = try review(Fixture.data("scan-report-groups"))
        let items0 = try #require(review.items)
        let before = items0.sidebar.map(\.entries)
        review.set(items0.rows.map(\.id), selected: false)
        review.toggle(try ReviewFixture.id(items0, "p1/node_modules"))
        #expect(try #require(review.items).sidebar.map(\.entries) == before)
        #expect(review.amount(of: ReviewItems.Entry(riskClass: .rebuildable, group: "node_modules")) == "588.0 of 784.0 KiB")
        #expect(review.amount(of: ReviewItems.Entry(riskClass: .rebuildable, group: "Python environments")) == "496.0 KiB")
    }

    /// Review venvs without a lock file, as the scan reports them.
    private func venvs() throws -> Data {
        var ino = 9_000
        func venv(_ path: String, _ bytes: Int, appOwned: Bool = false) throws -> [String: Any] {
            ino += 1
            return try ReviewFixture.finding(5, [
                "path": path, "bytes": bytes, "ino": ino, "class": "review", "rule_class": "rebuildable", "removal": "trash",
                "selected": false, "not_selected": ["reason": "review"], "notes": appOwned ? ["no-lockfile", "app-owned"] : ["no-lockfile"],
                "hint": "recreate: uv sync / pip install -r requirements.txt",
                "project": ["root": (path as NSString).deletingLastPathComponent, "last_activity": "2026-08-17T00:00:00Z", "idle_days": 63, "has_lockfile": false],
            ])
        }
        return try Fixture.editedFindings("scan-report-groups") { findings in
            findings += [
                try! venv("/Users/example/temp/autogen/.venv", 500_000),
                try! venv("/Users/example/temp/mcp/.venv", 100_000),
                try! venv("/Users/example/Projects/car/.venv", 300_000),
                try! venv("/Users/example/Projects/SmartAndPoint/tools/gdoc/.venv", 250_000),
                try! venv("/Users/example/.freecad-mcp/venv", 90_000, appOwned: true),
                try! venv("/Users/example/Library/Application Support/Zed/pylsp-venv", 95_000, appOwned: true),
                try! venv("/Volumes/Work/proj/.venv", 120_000),
                try! venv("/opt/tools/.venv", 80_000),
                try! venv("/Users/example/.venv", 70_000),
                try! venv("/Users/example/Caf\u{E9}/a/.venv", 66_000),
                try! venv("/Users/example/Cafe\u{301}/b/.venv", 67_000),
            ]
        }
    }

    @Test("one rule falls into folders: the first folder under home, a volume, the first component elsewhere; app-owned apart; paths relative")
    func foldersShape() throws {
        let items = try ReviewFixture.items(report: venvs())
        let layout = items.layout(ReviewItems.Entry(riskClass: .review, group: "Python environments"), matching: "")
        #expect(layout.shape == .folders)
        #expect(layout.facts == "python.venv · Move to the Trash · No lock file")
        #expect(layout.hint == "Recreate: uv sync / pip install -r requirements.txt")
        #expect(layout.count == 11)

        let folders = subgroups(layout)
        #expect(folders.map(\.title) == ["~/temp", "~/Projects", "Inside apps' own folders", "/Volumes/Work", "/opt", "~", "~/Cafe\u{301}", "~/Caf\u{E9}"])
        #expect(folders.map(\.bytes) == [600_000, 550_000, 185_000, 120_000, 80_000, 70_000, 67_000, 66_000])
        #expect(folders[0].items.map(\.shown) == ["autogen/.venv", "mcp/.venv"])
        #expect(folders[1].items.map(\.shown) == ["car/.venv", "SmartAndPoint/tools/gdoc/.venv"])
        #expect(folders[2].kind == .appOwned)
        #expect(folders[2].items.map(\.shown) == ["~/Library/Application Support/Zed/pylsp-venv", "~/.freecad-mcp/venv"])
        #expect(folders[3].items.map(\.shown) == ["proj/.venv"])
        #expect(folders[4].items.map(\.shown) == ["tools/.venv"])
        #expect(folders[5].items.map(\.shown) == [".venv"])
        #expect(folders[0].subtitle == "2 folders")
        #expect(folders[2].subtitle == "2 folders · the app may need it back")

        // Two spellings of one name stay two subgroups, with keys of their own.
        #expect(folders[6].key != folders[7].key)
        #expect(Set(folders.map(\.key)).count == folders.count)
        // The row says only what is its own.
        #expect(folders.flatMap(\.items).allSatisfy { $0.detail == "Untouched 63 days" })
        // A name that starts with a combining mark keeps it, apart from the "/".
        #expect(ReviewItems.folder(of: "/Users/example/\u{301}x/a/.venv", home: "/Users/example/") == ("~/\u{301}x", "a/.venv"))
        #expect(ReviewItems.folder(of: "/Users/example/.venv", home: "/Users/example") == ("~", ".venv"))
        #expect(ReviewItems.folder(of: "/Volumes/Work/p/.venv", home: "/Users/example") == ("/Volumes/Work", "p/.venv"))
        #expect(ReviewItems.folder(of: "/Volumes/.venv", home: "/Users/example") == ("/Volumes", ".venv"))
        #expect(ReviewItems.folder(of: "/.venv", home: "/Users/example") == ("/.venv", "/.venv"))

        // The filter removes items; the shape stays.
        let filtered = items.layout(ReviewItems.Entry(riskClass: .review, group: "Python environments"), matching: "temp")
        #expect(filtered.shape == .folders)
        #expect(subgroups(filtered).map(\.title) == ["~/temp"])
        #expect(filtered.selectAll?.count == 2)
    }

    /// Package caches with several rules, and Build outputs with two rules
    /// whose ids end the same.
    private func caches(scanRoots: [String]? = nil) throws -> Data {
        var ino = 9_500
        func cache(_ base: Int, _ rule: String, _ path: String, _ bytes: Int, hint: String = "") throws -> [String: Any] {
            ino += 1
            return try ReviewFixture.finding(base, ["rule": rule, "path": path, "bytes": bytes, "ino": ino, "hint": hint])
        }
        return try Fixture.edited("scan-report-groups") { json in
            var findings = json["findings"] as? [[String: Any]] ?? []
            findings += [
                try! cache(0, "cache.npm", "/Users/example/.npm/_logs", 200_000),
                try! cache(0, "cache.cargo", "/Users/example/.cargo/registry/cache", 210_000),
                try! cache(0, "cache.cargo", "/Users/example/.cargo/registry/src", 146_000),
                try! cache(0, "cache.uv", "/Users/example/.cache/uv", 812_000, hint: "refills on the next uv sync"),
                try! cache(6, "rust.target", "/Users/example/r2/target", 300_000),
                try! cache(6, "maven.target", "/Users/example/m1/target", 280_000),
                try! cache(6, "maven.target", "/Users/example/m2/target", 270_000),
            ]
            json["findings"] = findings
            if let scanRoots { json["scan_roots"] = scanRoots }
        }
    }

    @Test("several rules give a subgroup per rule, titled by the id's last part; a one-item rule is one row; equal titles keep their own keys")
    func rulesShape() throws {
        let items = try ReviewFixture.items(report: caches())
        let layout = items.layout(ReviewItems.Entry(riskClass: .safe, group: "Package caches"), matching: "")
        #expect(layout.shape == .rules)
        #expect(layout.facts == nil)
        #expect(layout.blocks.count == 3)
        guard case .subgroup(let npm) = layout.blocks[0], case .single(let uv) = layout.blocks[1], case .subgroup(let cargo) = layout.blocks[2] else {
            Issue.record("blocks out of order: \(layout.blocks)")
            return
        }
        #expect(npm.title == "npm" && npm.subtitle == "cache.npm · 2 folders · Delete" && npm.kind == .rule("cache.npm"))
        #expect(npm.items.map(\.shown) == ["~/.npm/_cacache", "~/.npm/_logs"])
        #expect(uv.title == "uv" && uv.subtitle == "cache.uv · Delete" && uv.item.shown == "~/.cache/uv")
        #expect(uv.hint == "Refills on the next uv sync" && uv.detail == "Refills on the next uv sync")
        #expect(cargo.title == "cargo" && cargo.items.map(\.row.finding.bytes) == [210_000, 146_000])

        let builds = items.layout(ReviewItems.Entry(riskClass: .rebuildable, group: "Build outputs"), matching: "")
        let targets = subgroups(builds)
        #expect(targets.map(\.title) == ["target", "target"])
        #expect(targets[0].key != targets[1].key)
        #expect(Set(targets.map(\.kind)) == [.rule("rust.target"), .rule("maven.target")])

        // A filter down to one rule keeps the rules shape.
        let cargoOnly = items.layout(ReviewItems.Entry(riskClass: .safe, group: "Package caches"), matching: "cargo")
        #expect(cargoOnly.shape == .rules)
        #expect(subgroups(cargoOnly).map(\.title) == ["cargo"] && singles(cargoOnly).isEmpty)
        // Down to one item: still a subgroup, counting what it shows.
        let src = subgroups(items.layout(ReviewItems.Entry(riskClass: .safe, group: "Package caches"), matching: "registry/src"))
        #expect(src.map(\.subtitle) == ["cache.cargo · 1 folder · Delete"] && src.map(\.bytes) == [146_000])
    }

    @Test("Select all and a heading are off, mixed or on over their selectable rows, and locked rows never change")
    func triState() throws {
        // Both npm folders are scan roots: locked.
        let review = try review(caches(scanRoots: ["/Users/example", "/Users/example/.npm/_cacache", "/Users/example/.npm/_logs"]))
        let items = try #require(review.items)
        let layout = items.layout(ReviewItems.Entry(riskClass: .safe, group: "Package caches"), matching: "")
        let all = try #require(layout.selectAll)
        let locked = Set(all.filter { items.row($0)?.selectable == false })
        #expect(locked.count == 2)

        review.set(all, selected: false)
        #expect(review.checkState(all) == .off)
        let elsewhere = try ReviewFixture.id(items, "p1/node_modules")
        let before = review.isSelected(elsewhere)
        review.toggleAll(all)
        #expect(review.checkState(all) == .on)
        #expect(review.selection.isDisjoint(with: locked))
        #expect(review.isSelected(elsewhere) == before, "Select all changes only its own group")

        let cargo = try #require(subgroups(layout).first { $0.title == "cargo" })
        review.toggle(cargo.ids[0])
        #expect(review.checkState(all) == .mixed)
        #expect(review.checkState(cargo.ids) == .mixed)
        review.toggleAll(cargo.ids)
        #expect(review.checkState(cargo.ids) == .on)
        review.toggleAll(cargo.ids)
        #expect(review.checkState(cargo.ids) == .off)
        #expect(review.isSelected(try ReviewFixture.id(items, ".cache/uv")), "a heading changes only its own rows")

        review.toggleAll(cargo.ids)
        #expect(review.checkState(all) == .on)
        review.toggleAll(all)
        #expect(review.checkState(all) == .off)

        // A subgroup with nothing selectable stays off, and its checkbox does nothing.
        let npm = try #require(subgroups(layout).first { $0.title == "npm" })
        #expect(!review.canSelect(npm.ids) && review.checkState(npm.ids) == .off)
        review.toggleAll(npm.ids)
        #expect(review.selection.isDisjoint(with: locked))
    }

    @Test("a row's detail holds none of its rule id, action, class, default note or way back; the headings keep them; the Items view's detail is unchanged")
    func ownDetail() throws {
        for report in [try venvs(), try caches()] {
            let items = try ReviewFixture.items(report: report)
            for entry in items.sidebar.flatMap(\.entries) {
                let layout = items.layout(entry, matching: "")
                let rows = layout.blocks.flatMap { block -> [GroupLayout.Item] in
                    switch block {
                    case .subgroup(let subgroup): subgroup.items
                    case .single(let single): [single.item]
                    case .entries(let items): items
                    }
                }
                for item in rows {
                    let detail = item.detail
                    #expect(!detail.contains(item.row.finding.rule), "\(detail)")
                    #expect(!ListedAction.allCases.map(CleanWording.action).contains(where: detail.contains), "\(detail)")
                    #expect(!ScanReport.RiskClass.allCases.map(CleanWording.className).contains(where: detail.contains), "\(detail)")
                    #expect(!detail.localizedCaseInsensitiveContains("not selected by default"), "\(detail)")
                    if let hint = CleanWording.hint(item.row.finding.hint) { #expect(!detail.contains(hint), "\(detail)") }
                }
            }
        }
        let caches = try ReviewFixture.items(report: caches())
        let npm = try #require(subgroups(caches.layout(ReviewItems.Entry(riskClass: .safe, group: "Package caches"), matching: "")).first)
        #expect(npm.subtitle.contains("cache.npm") && npm.subtitle.contains("Delete"))

        // The Items view keeps its full line.
        let items = try ReviewFixture.items()
        #expect(CleanWording.detail(try #require(items.row(4))) == "Idle 656 days · lock file")
        #expect(CleanWording.detail(try #require(items.row(8))) == "Review · not selected by default · Re-download with ollama pull; remove one model with ollama rm")
    }

    @Test("a folded subgroup shows its heading only, stays folded across groups and reloads, and opens again when the window closes")
    func collapse() throws {
        let review = try review(venvs())
        let items = try #require(review.items)
        let entry = ReviewItems.Entry(riskClass: .review, group: "Python environments")
        let layout = items.layout(entry, matching: "")
        #expect(layout.lines(collapsed: []).count == subgroups(layout).count + 11)

        let temp = subgroups(layout)[0]
        review.toggleCollapsed(temp.key)
        let lines = layout.lines(collapsed: review.collapsed)
        #expect(lines.count == subgroups(layout).count + 11 - temp.items.count)
        #expect(lines.first == .heading(temp, expanded: false))

        review.entry = ReviewItems.Entry(riskClass: .safe, group: "Package caches")
        review.reload()
        review.entry = entry
        #expect(review.collapsed == [temp.key])
        review.windowClosed()
        #expect(review.collapsed.isEmpty)
    }

    @Test("user data has no Select all and no heading checkbox")
    func userDataNoSelectAll() throws {
        let items = try ReviewFixture.items()
        #expect(items.sidebar.last?.entries.map(\.group) == ["Downloads", "Pictures"], "one entry per rule, largest first")
        for group in ["Downloads", "Pictures"] {
            let layout = items.layout(ReviewItems.Entry(riskClass: .userData, group: group), matching: "")
            #expect(layout.count == 1 && layout.shape == .folders, "a folder that lists no contents shows as itself")
            #expect(layout.selectAll == nil)
            #expect(subgroups(layout).allSatisfy { !$0.checkable })
        }
    }

    @Test("amounts read 'selected of total' only when part is selected, the unit said once when shared")
    func amount() {
        #expect(Wording.amount(selected: 0, total: 2048) == "2.0 KiB")
        #expect(Wording.amount(selected: 2048, total: 2048) == "2.0 KiB")
        #expect(Wording.amount(selected: 1024, total: 2048) == "1.0 of 2.0 KiB")
        #expect(Wording.amount(selected: 512, total: 2_097_152) == "512 B of 2.0 MiB")
        #expect(Wording.amount(selected: 2_097_100, total: 2_097_152) == "2.0 MiB", "never \"2.0 of 2.0 MiB\"")
    }
}
