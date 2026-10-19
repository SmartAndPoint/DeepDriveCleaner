import Foundation
import Testing
@testable import DDCKit

/// Story "Review and clean window": what can be selected, and the `--items`
/// payload. Temporary directories and defaults suites only.
@Suite("Review window: selection and payload")
@MainActor
struct ReviewSelectionTests {
    let data: TempDataDirectory
    let defaults: TempDefaults

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
    }

    @Test("the --items payload is exactly the confirmed list, in the sheet's order, with the CLI's keys only")
    func payloadIsTheConfirmedList() throws {
        let items = try ReviewFixture.items()
        let confirmation = DDCKit.Confirmation(items, selection: try ReviewFixture.recordedSelection(items), deletePermanently: false)
        let payload = try confirmation.itemList(checkedAgainst: items).encoded()

        let json = try #require(try JSONSerialization.jsonObject(with: payload) as? [String: Any])
        #expect(Set(json.keys) == ["schema_version", "items"])
        #expect(json["schema_version"] as? Int == 1)
        let listed = try #require(json["items"] as? [[String: Any]])
        #expect(listed.map { $0["path"] as? String } == confirmation.items.map(\.finding.path))
        #expect(listed.map { $0["path"] as? String } == [
            "/Users/example/.npm/_cacache", "/Users/example/Library/Caches/Google/Chrome",
            "/Users/example/p1/node_modules", "/Users/example/py1/.venv", "/Users/example/r1/target",
            "/Users/example/.ollama/models",
        ])
        for (item, confirmed) in zip(listed, confirmation.items) {
            #expect(Set(item.keys) == ["path", "dev", "ino", "rule", "class", "action"])
            #expect(item["dev"] as? UInt64 == confirmed.finding.dev)
            #expect(item["ino"] as? UInt64 == confirmed.finding.ino)
            #expect(item["rule"] as? String == confirmed.finding.rule)
            #expect(item["class"] as? String == confirmed.finding.class.rawValue)
        }
        #expect(listed.map { $0["action"] as? String } == ["delete", "delete", "delete", "delete", "delete", "trash"])
    }

    @Test("user data, ignored findings and runtimes ddc has no command for never reach the payload")
    func lockedNeverListed() throws {
        let colima = try ReviewFixture.finding(0, ["path": "/Users/example/.colima", "rule": "vm.colima", "class": "review", "rule_class": "review", "removal": "native-only", "selected": false, "not_selected": ["reason": "review"]])
        let podman = try ReviewFixture.finding(0, ["path": "/Users/example/.local/share/containers", "rule": "vm.podman", "class": "review", "rule_class": "review", "removal": "native-only", "selected": false, "not_selected": ["reason": "review"]])
        let report = try Fixture.editedFindings("scan-report-groups") { $0 += [colima, podman] }
        let items = try ReviewFixture.items(report: report)

        // A selection forged with every row: only the selectable ones are listed.
        let confirmation = DDCKit.Confirmation(items, selection: Set(items.rows.map(\.id)), deletePermanently: false)
        let paths = Set(confirmation.itemList(checkedAgainst: items).items.map(\.path))
        #expect(!paths.contains { $0.hasSuffix("Downloads") || $0.hasSuffix("Pictures") })
        #expect(!paths.contains { $0.hasSuffix("Caches/pip") || $0.hasSuffix(".npm/_npx") })
        #expect(!paths.contains("/Users/example/.colima"))
        #expect(paths.contains("/Users/example/.local/share/containers"))
        #expect(items.row(11)?.lock == .noAction)
        #expect(items.row(12)?.action == .nativeOnly)

        // Without the rules list, `switchable` is unknown: no native-only item is selectable.
        let bare = ReviewItems.make(report: try ScanReport.read(report), settings: try Fixture.groupsSettings(), rules: nil)
        #expect(bare.row(12)?.lock == .noAction)
        #expect(bare.row(0)?.group == ReportSummary.otherGroup)

        // An item locked after the sheet opened (protected since) is left out, never sent.
        let p1 = try ReviewFixture.id(items, "p1/node_modules")
        let selected = DDCKit.Confirmation(items, selection: [p1, 0], deletePermanently: false)
        let after = items.locking([p1], as: .protected)
        #expect(selected.itemList(checkedAgainst: after).items.map(\.path) == ["/Users/example/.npm/_cacache"])
    }

    @Test("paths and identities reach the payload byte for byte")
    func bytesSurvive() throws {
        let odd = "/Users/example/Cafe\u{301} \"q\" \\b/node_modules"
        let report = try Fixture.editedFindings("scan-report-groups") { findings in
            findings[4]["path"] = odd
            findings[4]["ino"] = NSNumber(value: UInt64.max - 7)
        }
        let items = try ReviewFixture.items(report: report)
        let confirmation = DDCKit.Confirmation(items, selection: [4], deletePermanently: false)
        let payload = try confirmation.itemList(checkedAgainst: items).encoded()

        struct Listed: Decodable { let items: [Item]; struct Item: Decodable { let path: String; let ino: UInt64 } }
        let decoded = try JSONDecoder().decode(Listed.self, from: payload)
        #expect(Array(decoded.items[0].path.utf8) == Array(odd.utf8), "NFD stays NFD")
        #expect(decoded.items[0].ino == UInt64.max - 7)
        let text = String(decoding: payload, as: UTF8.self)
        #expect(text.contains(#""path":"/Users/example/"#), "slashes unescaped")
        #expect(payload.contains(Data([0x65, 0xCC, 0x81])), "e + combining acute, as decoded")
    }

    @Test("a review item's permanent starts from the preference; one switch changes one item; a tool's item has none")
    func permanentFromPreference() throws {
        let second = try ReviewFixture.finding(8, ["path": "/Users/example/.cache/huggingface", "rule": "model.huggingface", "ino": 1])
        let podman = try ReviewFixture.finding(8, ["path": "/Users/example/.local/share/containers", "rule": "vm.podman", "removal": "native-only", "ino": 2])
        let report = try Fixture.editedFindings("scan-report-groups") { $0 += [second, podman] }
        let items = try ReviewFixture.items(report: report)
        let ids: Set<Int> = [8, 11, 12]

        let off = DDCKit.Confirmation(items, selection: ids, deletePermanently: false)
        let offList = try listed(off, items)
        #expect(offList.map { $0["action"] as? String } == ["trash", "trash", "native-only"])
        #expect(offList.allSatisfy { $0["permanent"] == nil })

        var on = DDCKit.Confirmation(items, selection: ids, deletePermanently: true)
        #expect(try listed(on, items).map { $0["action"] as? String } == ["delete", "delete", "native-only"])
        #expect(try listed(on, items).map { $0["permanent"] as? Bool } == [true, true, nil])
        #expect(on.items.map(\.canBePermanent) == [true, true, false])

        on.togglePermanent(on.items[1].id)
        #expect(try listed(on, items).map { $0["action"] as? String } == ["delete", "trash", "native-only"])
        on.togglePermanent(on.items[2].id)
        #expect(try listed(on, items)[2]["permanent"] == nil, "a tool's item has no switch")
    }

    private func listed(_ confirmation: DDCKit.Confirmation, _ items: ReviewItems) throws -> [[String: Any]] {
        let json = try JSONSerialization.jsonObject(with: confirmation.itemList(checkedAgainst: items).encoded()) as? [String: Any]
        return try #require(json?["items"] as? [[String: Any]])
    }

    @Test("user data and ignored findings cannot be selected; review findings start unselected")
    func locks() throws {
        let items = try ReviewFixture.items()
        #expect(items.row(9)?.lock == .userData && items.row(10)?.lock == .userData)
        #expect(items.row(1)?.lock == .typeOff)
        #expect(items.row(3)?.lock == .underMinimumSize)
        #expect(items.row(8)?.lock == nil && items.row(8)?.defaultSelected == false)
        #expect(items.defaultSelection(classes: [.safe, .rebuildable, .review]) == [0, 2, 4, 5, 6])

        let model = makeReview(runner: nil, data: data, defaults: defaults) { try? ReviewFixture.snapshot(settingsIn: data) }
        #expect(model.open(ReviewRequest(group: nil, classes: [.safe, .rebuildable], confirm: false)))
        for id in [1, 3, 9, 10] {
            model.toggle(id)
            #expect(!model.isSelected(id))
        }
        model.set(Array(0...10), selected: true)
        #expect(model.selection == [0, 2, 4, 5, 6, 7, 8], "select all skips what cannot be cleaned")

        // A minimum size raised, or a type switched off, after the scan takes effect at once.
        let raised = try SharedSettings.read(at: {
            try data.write(settings: #"{"schema_version": 1, "min_size": 500000, "disabled_rules": ["app.chrome-cache"]}"#)
            return data.directory.settings
        }())
        let now = ReviewItems.make(report: try Fixture.groupsReport(), settings: raised, rules: try Fixture.rules())
        #expect(now.row(6)?.lock == .underMinimumSize)
        #expect(now.row(2)?.lock == .typeOff)
        #expect(now.row(0)?.lock == nil)

        // A scan root itself is never planned.
        let rooted = try Fixture.edited("scan-report-groups") { $0["scan_roots"] = ["/Users/example", "/Users/example/.npm/_cacache"] }
        #expect(try ReviewFixture.items(report: rooted).row(0)?.lock == .scanRoot)
    }

    @Test("Protect this folder writes .ddc-keep only into the folder the scan saw, and locks its row")
    func protect() throws {
        let folder = data.url.appending(path: "project/node_modules")
        try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
        let (dev, ino) = try identity(folder)
        let marker = folder.appending(path: ".ddc-keep")

        try KeepMarker.protect(path: folder.path, dev: dev, ino: ino)
        #expect(FileManager.default.fileExists(atPath: marker.path))
        #expect(try Data(contentsOf: marker).isEmpty)
        try KeepMarker.protect(path: folder.path, dev: dev, ino: ino)  // already there: fine

        let swapped = data.url.appending(path: "swapped")
        try FileManager.default.createDirectory(at: swapped, withIntermediateDirectories: true)
        let (sdev, sino) = try identity(swapped)
        try FileManager.default.removeItem(at: swapped)
        try FileManager.default.createDirectory(at: swapped.appending(path: "x"), withIntermediateDirectories: true)
        try FileManager.default.moveItem(at: swapped.appending(path: "x"), to: data.url.appending(path: "x"))
        #expect(throws: KeepMarker.Failure.changed) { try KeepMarker.protect(path: swapped.path, dev: sdev, ino: sino) }
        #expect(!FileManager.default.fileExists(atPath: swapped.appending(path: ".ddc-keep").path))

        let link = data.url.appending(path: "link")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: folder)
        #expect(throws: KeepMarker.Failure.changed) { try KeepMarker.protect(path: link.path, dev: dev, ino: ino) }
        #expect(throws: KeepMarker.Failure.missing) { try KeepMarker.protect(path: data.url.appending(path: "gone").path, dev: dev, ino: ino) }

        // Through the window: the row can no longer be selected.
        let other = data.url.appending(path: "other/node_modules")
        try FileManager.default.createDirectory(at: other, withIntermediateDirectories: true)
        let (odev, oino) = try identity(other)
        let report = try Fixture.editedFindings("scan-report-groups") { findings in
            findings[4]["path"] = other.path
            findings[4]["dev"] = NSNumber(value: odev)
            findings[4]["ino"] = NSNumber(value: oino)
        }
        let model = makeReview(runner: nil, data: data, defaults: defaults) { try? ReviewFixture.snapshot(report: report, settingsIn: data) }
        _ = model.open(ReviewRequest(group: nil, classes: [.safe, .rebuildable], confirm: false))
        #expect(model.isSelected(4))
        model.protect(4)
        #expect(FileManager.default.fileExists(atPath: other.appending(path: ".ddc-keep").path))
        #expect(model.items?.row(4)?.lock == .protected)
        #expect(!model.isSelected(4))
        model.toggle(4)
        #expect(!model.isSelected(4))
        model.reload()
        #expect(model.items?.row(4)?.lock == .protected, "until a scan reports it as protected")
    }

    private func identity(_ url: URL) throws -> (UInt64, UInt64) {
        var info = stat()
        #expect(lstat(url.path, &info) == 0)
        return (UInt64(info.st_dev), UInt64(info.st_ino))
    }
}
