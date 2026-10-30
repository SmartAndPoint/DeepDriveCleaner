import Foundation
import Testing
@testable import DDCKit

/// The groups report with the user's own folders listing entries (story
/// "User data: pick your own files for the Trash"). Findings 0 to 10 as in
/// `ReviewFixture`, then 11, a `node_modules` inside `~/Downloads/Project`.
/// Downloads lists four entries and Pictures two; their rows follow. With
/// `movies`, 12 Movies (two entries and the iMovie library), 13 Telegram and
/// 14 WhatsApp (no entries) come before them.
enum UserDataFixture {
    static let home = "/Users/example"
    static let downloadsBytes: UInt64 = 3_000_000_000

    static func report(skipped: [(path: String, reason: String)] = [], movies: Bool = false) throws -> Data {
        var json = try #require(try JSONSerialization.jsonObject(with: Fixture.data("scan-report-groups")) as? [String: Any])
        var findings = try #require(json["findings"] as? [[String: Any]])
        var nested = findings[4]
        nested["path"] = "\(home)/Downloads/Project/node_modules"
        nested["ino"] = 9_001
        findings.append(nested)
        findings[9]["bytes"] = downloadsBytes
        findings[9]["contents"] = [
            "entries": [
                entry("Downloads/big.iso", "file", 50_000_000, ino: 101, ["modified": "2033-05-08T03:33:20Z"]),
                entry("Downloads/Project", "folder", 30_000_000, ino: 102, ["findings": 1, "git": true]),
                entry("Downloads/cod\u{202E}fdp.exe", "file", 20_000_000, ino: 103),
                entry("Downloads/movie.part", "file", 15_000_000, ino: 104, ["lock": ["reason": "partial-download"]]),
            ],
            "rest_count": 1_904,
            "rest_bytes": 2_469_606_195,
        ]
        findings[10]["contents"] = [
            "entries": [
                entry("Pictures/Trip", "folder", 40_000_000, ino: 201),
                entry("Pictures/Photos Library.photoslibrary", "folder", 30_000_000, ino: 202, ["lock": ["reason": "app-managed", "app": "Photos"]]),
            ],
            "rest_count": 0,
            "rest_bytes": 0,
        ]
        if movies {
            var folder = findings[10]
            folder.removeValue(forKey: "contents")
            for (n, name) in ["Movies", "Telegram", "WhatsApp"].enumerated() {
                folder["path"] = "\(home)/\(name)"
                folder["rule"] = "user.\(name.lowercased())"
                folder["ino"] = 300 + n
                findings.append(folder)
            }
            findings[12]["contents"] = [
                "entries": [
                    entry("Movies/Holiday.mov", "file", 2_000_000_000, ino: 301),
                    entry("Movies/Export", "folder", 900_000_000, ino: 302),
                    entry("Movies/iMovie Library.imovielibrary", "folder", 500_000_000, ino: 303, ["lock": ["reason": "app-managed", "app": "iMovie"]]),
                ],
                "rest_count": 0,
                "rest_bytes": 0,
            ]
        }
        json["findings"] = findings
        json["skipped"] = skipped.map { ["path": "\(home)/\($0.path)", "reason": $0.reason] }
        return try JSONSerialization.data(withJSONObject: json)
    }

    static func items(skipped: [(path: String, reason: String)] = [], movies: Bool = false) throws -> ReviewItems {
        ReviewItems.make(report: try ScanReport.read(report(skipped: skipped, movies: movies)), settings: try Fixture.groupsSettings(), rules: try Fixture.rules())
    }

    private static func entry(_ rel: String, _ kind: String, _ bytes: UInt64, ino: Int, _ extra: [String: Any] = [:]) -> [String: Any] {
        ["path": "\(home)/\(rel)", "kind": kind, "bytes": bytes, "dev": 1, "ino": ino].merging(extra) { _, new in new }
    }
}

@Suite("Review window: the user's own files")
@MainActor
struct UserDataTests {
    private func id(_ items: ReviewItems, _ suffix: String) throws -> Int {
        try #require(items.rows.last { $0.finding.path.hasSuffix(suffix) }).id
    }

    @Test("an entry is selectable only when listed and unlocked, never by default, and the folder itself never")
    func selectable() throws {
        let items = try UserDataFixture.items()
        let entries = items.rows.filter { $0.userEntry != nil }
        #expect(entries.map(\.id) == Array(12..<18), "ids after the findings'")
        #expect(entries.allSatisfy { $0.riskClass == .userData && $0.action == .trash && !$0.defaultSelected })
        #expect(items.row(try id(items, "big.iso"))?.selectable == true)
        #expect(items.row(try id(items, "movie.part"))?.lock == .entry(.partialDownload))
        #expect(items.row(try id(items, ".photoslibrary"))?.lock == .entry(.appManaged(app: "Photos")))
        #expect(items.row(try id(items, "/Downloads"))?.lock == .userData)
        #expect(items.defaultSelection(classes: Set(ScanReport.RiskClass.allCases)).isDisjoint(with: entries.map(\.id)))
        #expect(items.items(matching: "", sort: .size).allSatisfy { $0.userEntry == nil }, "not in the Items view")
    }

    @Test("a user-data group lists its entries largest first, with no Select all, the rest as one line, names escaped")
    func layout() throws {
        let items = try UserDataFixture.items()
        #expect(items.sidebar.last?.riskClass == .userData)
        #expect(items.sidebar.last?.entries.map(\.group) == ["Downloads", "Pictures"])
        let layout = items.layout(ReviewItems.Entry(riskClass: .userData, group: "Downloads"), matching: "")
        #expect(layout.shape == .entries && layout.selectAll == nil)
        let lines = layout.lines(collapsed: [])
        let shown = lines.compactMap { line -> String? in if case .entry(let item) = line { item.shown } else { nil } }
        #expect(shown == ["big.iso", "Project", "cod\\u{202E}fdp.exe", "movie.part"])
        #expect(lines.last == .note("1,904 smaller items (2.3 GiB) are not listed"))
        let details = lines.compactMap { line -> String? in if case .entry(let item) = line { item.detail } else { nil } }
        #expect(details[0].contains("Last changed"))
        #expect(details[1].contains("Holds 1 finding of its own") && details[1].contains("A project (has .git)"))
        #expect(details[3].hasPrefix("Still downloading"))
        #expect(!layout.lines(collapsed: []).contains { if case .heading = $0 { true } else { false } })
    }

    @Test("every entry lock decodes and reads as text; an unknown one does not decode")
    func locks() throws {
        for tag in EntryLock.tags {
            let lock = try JSONDecoder().decode(EntryLock.self, from: Data(#"{"reason":"\#(tag)","app":"Photos"}"#.utf8))
            #expect(!CleanWording.entryLock(lock).isEmpty)
            #expect(ReviewItems.Lock.entry(lock).text == CleanWording.entryLock(lock))
        }
        #expect(throws: (any Error).self) { try JSONDecoder().decode(EntryLock.self, from: Data(#"{"reason":"new-lock"}"#.utf8)) }
    }

    @Test("a backup row says its device and the date of its last backup")
    func backupDetail() throws {
        var json = try #require(try JSONSerialization.jsonObject(with: UserDataFixture.report()) as? [String: Any])
        var findings = try #require(json["findings"] as? [[String: Any]])
        findings[10]["rule"] = "user.ios-backups"
        findings[10]["contents"] = [
            "entries": [[
                "path": "/Users/example/Pictures/00008110-000A", "kind": "folder", "bytes": 40_000_000, "dev": 1, "ino": 501,
                "modified": "2026-10-01T12:30:45Z",
                "backup": ["device_name": "Example's iPhone", "last_backup": "2026-10-01T12:30:45Z"],
            ]],
            "rest_count": 0, "rest_bytes": 0,
        ]
        json["findings"] = findings
        let items = ReviewItems.make(report: try ScanReport.read(JSONSerialization.data(withJSONObject: json)), settings: nil, rules: try Fixture.rules())
        let entry = try #require(items.sidebar.last?.entries.first { $0.group == "iPhone and iPad backups" })
        let lines = items.layout(entry, matching: "").lines(collapsed: [])
        guard case .entry(let item)? = lines.first else { Issue.record("no entry row"); return }
        #expect(item.detail.hasPrefix("Example's iPhone · Last backup"), "\(item.detail)")
        #expect(!item.detail.contains("Last changed"), "a backup says its last backup instead")
    }

    @Test("names with control or bidirectional format characters are shown escaped")
    func escaped() {
        #expect(Wording.escaped("cod\u{202E}fdp.exe") == "cod\\u{202E}fdp.exe")
        for scalar in ["\u{202A}", "\u{2066}", "\u{2069}", "\u{200E}", "\u{200F}", "\u{061C}", "\u{1B}"] {
            #expect(!Wording.escaped("a\(scalar)b").unicodeScalars.contains(scalar.unicodeScalars.first!))
        }
        #expect(Wording.escaped("Fotos 2024 – Café") == "Fotos 2024 – Café")
    }

    @Test("the item list sends class user-data, action trash and never permanent, whatever the preference")
    func listed() throws {
        let items = try UserDataFixture.items()
        let pick = try id(items, "big.iso")
        let confirmation = Confirmation(items, selection: [pick], deletePermanently: true)
        let list = confirmation.itemList(checkedAgainst: items)
        let item = try #require(list.items.first)
        #expect(item.class == .userData && item.action == .trash && !item.permanent && item.rule == "user.downloads")
        #expect(item.ino == 101 && item.path == "\(UserDataFixture.home)/Downloads/big.iso")
        let json = String(decoding: try list.encoded(), as: UTF8.self)
        #expect(!json.contains("permanent"))
    }

    @Test("the sheet lists the user's own files first, and a finding inside a picked folder only as included")
    func confirmation() throws {
        let items = try UserDataFixture.items()
        let project = try id(items, "Downloads/Project")
        let nested = try id(items, "Project/node_modules")
        let cache = try id(items, ".npm/_cacache")
        let confirmation = Confirmation(items, selection: [project, nested, cache], deletePermanently: false)
        #expect(confirmation.items.map(\.id) == [project, cache])
        #expect(confirmation.included == 1)
        #expect(confirmation.classRows.map(\.riskClass) == [.userData, .safe])
        #expect(confirmation.itemList(checkedAgainst: items).items.map(\.path).allSatisfy { !$0.hasSuffix("node_modules") })
    }

    @Test("Downloads' selected of total leaves out the findings inside a picked folder and never exceeds the total")
    func amounts() throws {
        let data = try TempDataDirectory()
        let defaults = try TempDefaults()
        let snapshot = ReviewSnapshot(report: try ScanReport.read(UserDataFixture.report()), rules: try Fixture.rules())
        let review = makeReview(runner: nil, data: data, defaults: defaults) { snapshot }
        _ = review.open(ReviewRequest(group: nil, classes: [], confirm: false))
        let items = try #require(review.items)
        let downloads = ReviewItems.Entry(riskClass: .userData, group: "Downloads")
        #expect(review.amount(of: downloads) == ByteSize.human(UserDataFixture.downloadsBytes))

        let project = try id(items, "Downloads/Project")
        let nested = try id(items, "Project/node_modules")
        review.toggle(project)
        review.toggle(nested)
        let own: UInt64 = 30_000_000 - 602_112
        #expect(review.amount(of: downloads) == Wording.amount(selected: own, total: UserDataFixture.downloadsBytes))
        #expect(review.selectedBytes == 30_000_000, "the footer counts the folder once")

        // Pictures' finding is far smaller than its listed entry here: the amount stops at the total.
        let pictures = ReviewItems.Entry(riskClass: .userData, group: "Pictures")
        review.toggle(try id(items, "Pictures/Trip"))
        #expect(review.amount(of: pictures) == ByteSize.human(53_248))
    }

    @Test("Protect this folder is offered for folder entries, not for files or inside a device backup")
    func protect() throws {
        let items = try UserDataFixture.items()
        #expect(items.canProtect(try #require(items.row(try id(items, "Downloads/Project")))))
        #expect(!items.canProtect(try #require(items.row(try id(items, "big.iso")))))

        // The same folder as a device backup.
        var json = try #require(try JSONSerialization.jsonObject(with: UserDataFixture.report()) as? [String: Any])
        var findings = try #require(json["findings"] as? [[String: Any]])
        findings[10]["rule"] = "user.ios-backups"
        json["findings"] = findings
        let backups = ReviewItems.make(report: try ScanReport.read(JSONSerialization.data(withJSONObject: json)), settings: nil, rules: try Fixture.rules())
        #expect(!backups.canProtect(try #require(backups.row(try id(backups, "Pictures/Trip")))))
    }

    @Test("entries never reach the popover's Selected to clean, Biggest finds or notification amounts")
    func popover() throws {
        let data = try UserDataFixture.report()
        var json = try #require(try JSONSerialization.jsonObject(with: data) as? [String: Any])
        json["findings"] = try #require(json["findings"] as? [[String: Any]]).map { $0.filter { $0.key != "contents" } }
        let settings = try Fixture.groupsSettings()
        let summary = ReportSummary.make(report: try ScanReport.read(data), settings: settings, rules: try Fixture.rules())
        let without = ReportSummary.make(
            report: try ScanReport.read(JSONSerialization.data(withJSONObject: json)), settings: settings, rules: try Fixture.rules()
        )
        #expect(summary.classes == without.classes)
        #expect(summary.topGroups == without.topGroups)
        #expect(summary.reclaimable == without.reclaimable)
        let userData = try #require(summary.classes.first { $0.riskClass == .userData })
        #expect(userData.selectedCount == 0 && userData.selectedBytes == 0)
    }

    @Test("the cloud folders come from the skipped areas, by path; a background scan's skips give the Scan-now line")
    func skippedAreas() throws {
        let background = try UserDataFixture.items(skipped: [("Downloads", "excluded"), ("Library/Containers", "excluded")])
        #expect(background.backgroundSkipped)
        #expect(background.cloudSkipped.isEmpty)
        let groups = background.sidebar.last?.entries.map(\.group) ?? []
        #expect(groups == ["Downloads", "Pictures"], "no Cloud folders entry")

        let cloud = try UserDataFixture.items(skipped: [("Library/CloudStorage", "excluded"), ("Library/Mobile Documents", "excluded")])
        #expect(!cloud.backgroundSkipped)
        let entry = try #require(cloud.sidebar.last?.entries.last)
        #expect(entry.group == CleanWording.cloudFolders)
        let layout = cloud.layout(entry, matching: "")
        #expect(layout.shape == .notScanned && layout.blurb == CleanWording.cloudBanner)
        #expect(layout.notScanned == ["~/Library/CloudStorage", "~/Library/Mobile Documents"])

        let data = try TempDataDirectory()
        let defaults = try TempDefaults()
        let snapshot = ReviewSnapshot(report: cloud.report, rules: try Fixture.rules())
        let review = makeReview(runner: nil, data: data, defaults: defaults) { snapshot }
        _ = review.open(ReviewRequest(group: nil, classes: [], confirm: false))
        #expect(review.amount(of: entry) == "not scanned")
    }

    @Test("a report with a skip reason this app does not know is unreadable")
    func strictSkips() throws {
        #expect(throws: (any Error).self) { try UserDataFixture.items(skipped: [("x", "new-reason")]) }
        var json = try #require(try JSONSerialization.jsonObject(with: UserDataFixture.report()) as? [String: Any])
        var findings = try #require(json["findings"] as? [[String: Any]])
        findings[10]["contents"] = ["entries": [["path": "/Users/example/Pictures/x", "kind": "folder", "bytes": 1, "dev": 1, "ino": 1, "lock": ["reason": "new-lock"]]], "rest_count": 0, "rest_bytes": 0]
        json["findings"] = findings
        let data = try JSONSerialization.data(withJSONObject: json)
        #expect(throws: (any Error).self) { try ScanReport.read(data) }
    }

    @Test("after a clean, a finding inside a trashed folder shows as cleaned")
    func cleanedInside() async throws {
        let data = try TempDataDirectory()
        let defaults = try TempDefaults()
        let report = try UserDataFixture.report()
        try data.write(lastScan: report)
        let fake = try FakeCLI(lastScan: data.directory.lastScan)
        let home = UserDataFixture.home
        try fake.put("dry-run.jsonl", Data("""
            {"type":"item","path":"\(home)/Downloads/Project","rule":"user.downloads","class":"user-data","action":"trash","bytes":30000000,"prediction":"would-trash"}
            {"type":"summary","items":1,"would_delete":0,"would_trash":1,"would_run":0,"would_skip":0,"bytes":30000000}

            """.utf8))
        try fake.put("clean-report.json", Data("""
            {"schema_version": 4, "ddc_version": "0.2.0", "platform": "macos", "capabilities": ["private-size", "in-use-check", "dataless-files", "full-disk-access", "local-snapshots", "whole-disk", "coding-agents", "homebrew", "schedule", "notifications"], "cancelled": false, "failed": 0, "local_snapshots": 0,
             "journal": "/Users/example/Library/Logs/DeepDriveCleaner/actions.jsonl", "volumes": [],
             "items": [{"path": "\(home)/Downloads/Project", "dev": 1, "ino": 102, "rule": "user.downloads",
                        "class": "user-data", "action": "trash", "bytes": 30000000, "outcome": "trashed"}]}
            """.utf8))
        try fake.put("saved-scan.json", report)
        let events = ReviewEvents()
        let snapshot = ReviewSnapshot(report: try ScanReport.read(report), rules: try Fixture.rules())
        let review = makeReview(runner: CLIRunner(executable: fake.executable), data: data, defaults: defaults, events: events) { snapshot }
        _ = review.open(ReviewRequest(group: nil, classes: [], confirm: false))
        let items = try #require(review.items)
        let project = try id(items, "Downloads/Project")
        let nested = try id(items, "Project/node_modules")
        review.selection = [project, nested]

        review.openSheet()
        try await until("the dry run") { review.sheet?.warnings == .checked }
        #expect(review.sheet?.predictions[project] == .wouldTrash)
        review.cleanConfirmedItems()
        try fake.open("clean-start")
        try fake.open("clean-go")
        try await until("the result") { review.sheet?.stage.result != nil }
        try await until("the rescan") { events.busy == [true, false] }
        review.closeSheet()
        #expect(review.items?.row(project)?.lock == .cleaned)
        #expect(review.items?.row(nested)?.lock == .cleaned)
    }
}
