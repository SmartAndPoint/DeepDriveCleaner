import Foundation
import Testing
@testable import DDCKit

/// Story "Protected by you: say what it is, and Remove Protection": what a
/// kept finding says protects it, and Remove Protection… on its row.
///
/// The fixture's kept findings, by index: 0 `~/p/node_modules` (its own
/// marker and one inside, rule `node.node_modules`), 1 `~/q/node_modules`
/// (holds a folder skipped in Settings, same rule), 2 `~/r/node_modules`
/// (excluded by a flag, no rule).
enum ProtectionFixture {
    static let home = "/Users/example"

    static func marker(_ folder: String = "/Users/example/p/node_modules", dev: UInt64 = 16_777_232, ino: UInt64 = 339_569_697) -> [String: Any] {
        ["folder": folder, "dev": dev, "ino": ino]
    }

    static func exclusion(_ path: String, kind: String, origin: String, inside: Bool) -> [String: Any] {
        ["path": path, "kind": kind, "origin": origin, "inside": inside]
    }

    static func protection(markers: [[String: Any]] = [], exclusions: [[String: Any]] = [], rule: String? = nil) -> [String: Any] {
        var protection: [String: Any] = ["markers": markers, "exclusions": exclusions]
        if let rule { protection["rule"] = rule }
        return protection
    }

    /// The fixture with finding `index`'s protection replaced.
    static func report(_ index: Int = 0, _ protection: [String: Any]) throws -> Data {
        try Fixture.editedFindings("scan-report-protection") { $0[index]["protection"] = protection }
    }

    static func finding(_ protection: [String: Any]) throws -> ScanReport.Finding {
        try ScanReport.read(try report(0, protection)).findings[0]
    }

    /// The report with finding 0 protected by its own marker only: a row
    /// that offers Remove Protection….
    static func oneMarker() throws -> Data {
        try report(0, protection(markers: [marker()], rule: "node.node_modules"))
    }

    static func snapshot(_ data: Data? = nil) throws -> ReviewSnapshot {
        ReviewSnapshot(report: try ScanReport.read(data ?? Fixture.data("scan-report-protection")), rules: try Fixture.rules())
    }

    static func keep(_ result: String, extra: String = "") -> CLIRunner.Result {
        let json = #"{"schema_version": 1, "path": "/Users/example/p/node_modules", "dev": 16777232, "ino": 339569697, "rule": "node.node_modules", "result": "\#(result)"\#(extra)}"#
        return FakeRunner.done(Data(json.utf8))
    }
}

/// A runner whose `keep remove` answers only when the test releases it;
/// every other run returns done and empty.
actor HeldKeep: CLIRunning {
    private let reply: CLIRunner.Result
    private var gate: CheckedContinuation<Void, Never>?
    private var released = false
    private(set) var runs = 0

    init(_ reply: CLIRunner.Result) {
        self.reply = reply
    }

    func run(_ kind: CLIRunner.Kind, _ arguments: [String], options: CLIRunner.Options) async -> CLIRunner.Result {
        guard kind == .keepRemove else { return FakeRunner.done(Data()) }
        runs += 1
        if !released { await withCheckedContinuation { gate = $0 } }
        return reply
    }

    func cancel(_ ticket: CLIRunner.Ticket) async {}

    func release() {
        released = true
        gate?.resume()
        gate = nil
    }
}

@Suite("Protected by you")
struct ProtectionTests {
    typealias F = ProtectionFixture

    // MARK: - The report

    @Test("the kept findings' protection decodes: markers, exclusions, origins and the rule")
    func decodes() throws {
        let report = try ScanReport.read(try Fixture.data("scan-report-protection"))
        let p = report.findings.map(\.protection)
        #expect(p[0]?.markers.map(\.folder) == ["/Users/example/p/node_modules", "/Users/example/p/node_modules/patched"])
        #expect(p[0]?.markers.first?.ino == 339_569_697 && p[0]?.rule == "node.node_modules")
        #expect(p[1]?.exclusions == [ScanReport.Exclusion(path: "/Users/example/q/node_modules/raw", kind: .skip, origin: .settings, inside: true)])
        #expect(p[2]?.exclusions.first?.origin == .flag && p[2]?.rule == nil)
        let plain = try ScanReport.read(try Fixture.data("scan-report"))
        #expect(plain.findings.allSatisfy { $0.protection == nil })
    }

    @Test("a kind or an origin this app does not know is a decoding error, as for Note")
    func strictEnums() throws {
        for (key, value) in [("kind", "ignore"), ("origin", "environment")] {
            var exclusion = F.exclusion("/Users/example/q/node_modules/raw", kind: "skip", origin: "settings", inside: true)
            exclusion[key] = value
            #expect(throws: DecodingError.self, "\(key) \(value)") {
                try ScanReport.read(try F.report(1, F.protection(exclusions: [exclusion])))
            }
        }
        let missing = try Fixture.editedFindings("scan-report-protection") { $0[0]["protection"] = ["markers": []] }
        #expect(throws: DecodingError.self, "exclusions is required") { try ScanReport.read(missing) }
        let null = try F.report(0, ["markers": [F.marker()], "exclusions": [], "rule": NSNull()])
        #expect(try ScanReport.read(null).findings[0].protection?.rule == nil)
    }

    // MARK: - The row's words

    @Test("each kind of protection reads as the story words it, with and without the rule")
    func rowWording() throws {
        func words(_ protection: [String: Any]) throws -> [String] {
            CleanWording.protection(try F.finding(protection), home: F.home)
        }
        let own = "/Users/example/p/node_modules"
        let held = "/Users/example/p/node_modules/raw"
        let rule = "node.node_modules"
        let cases: [([String: Any], [String])] = [
            (F.protection(markers: [F.marker()]), ["Protected by .ddc-keep in ~/p/node_modules"]),
            (F.protection(exclusions: [F.exclusion(own, kind: "exclude", origin: "settings", inside: false)]), ["Excluded in Settings"]),
            (F.protection(exclusions: [F.exclusion(own, kind: "exclude", origin: "flag", inside: false)]),
             ["Excluded by a command-line flag", CleanWording.flagOnly]),
            (F.protection(exclusions: [F.exclusion(held, kind: "skip", origin: "settings", inside: true)]),
             ["Holds a folder skipped in Settings: ~/p/node_modules/raw"]),
            (F.protection(exclusions: [F.exclusion(held, kind: "skip", origin: "flag", inside: true)]),
             ["Holds a folder skipped by a command-line flag: ~/p/node_modules/raw", CleanWording.flagOnly]),
            (F.protection(exclusions: [F.exclusion(held, kind: "exclude", origin: "settings", inside: true)]),
             ["Holds an excluded folder: ~/p/node_modules/raw"]),
        ]
        for (protection, expected) in cases {
            #expect(try words(protection) == expected)
            var ruled = protection
            ruled["rule"] = rule
            // The rule comes after the protections, and before how to remove them.
            let withRule = expected.contains(CleanWording.flagOnly)
                ? expected.dropLast() + ["otherwise: node.node_modules", CleanWording.flagOnly]
                : expected + ["otherwise: node.node_modules"]
            #expect(try words(ruled) == withRule)
        }
        // Several: all of them, and where to remove them.
        let several = F.protection(
            markers: [F.marker(), F.marker("\(own)/patched", ino: 339_569_712)],
            exclusions: [F.exclusion(held, kind: "skip", origin: "settings", inside: true)],
            rule: rule
        )
        #expect(try words(several) == [
            "Protected by .ddc-keep in ~/p/node_modules", "Protected by .ddc-keep in ~/p/node_modules/patched",
            "Holds a folder skipped in Settings: ~/p/node_modules/raw", "otherwise: node.node_modules", CleanWording.severalProtections,
        ])
    }

    @Test("a row kept by a .ddc-keep and a flag is told to remove both, never sent to Settings")
    func markerAndFlagWording() throws {
        func words(_ protection: [String: Any]) throws -> [String] {
            CleanWording.protection(try F.finding(protection), home: F.home)
        }
        let held = "/Users/example/p/node_modules/raw"
        let mixed = F.protection(markers: [F.marker()], exclusions: [F.exclusion(held, kind: "skip", origin: "flag", inside: true)])
        #expect(try words(mixed) == [
            "Protected by .ddc-keep in ~/p/node_modules", "Holds a folder skipped by a command-line flag: ~/p/node_modules/raw",
            "Remove the .ddc-keep in Finder and scan without the flag",
        ])
        #expect(CleanWording.markerAndFlag == "Remove the .ddc-keep in Finder and scan without the flag")
        #expect(try !words(mixed).contains(CleanWording.flagOnly))
        #expect(try !words(mixed).contains(CleanWording.severalProtections))
        // With its rule, the rule still comes before the advice.
        let ruled = F.protection(markers: [F.marker()], exclusions: [F.exclusion(held, kind: "skip", origin: "flag", inside: true)], rule: "node.node_modules")
        #expect(try words(ruled).suffix(2) == ["otherwise: node.node_modules", CleanWording.markerAndFlag])
        // A flag and a Settings exclusion, no marker: the flag still governs.
        let flagAndSettings = F.protection(exclusions: [
            F.exclusion(held, kind: "skip", origin: "flag", inside: true),
            F.exclusion("/Users/example/p/node_modules/other", kind: "skip", origin: "settings", inside: true),
        ])
        #expect(try words(flagAndSettings).last == CleanWording.flagOnly)
    }

    @Test("the rule is named by its whole id: several rules share a last part")
    func ruleNames() {
        #expect(CleanWording.ruleName("rust.target") == "rust.target")
        #expect(CleanWording.ruleName("rust.target") != CleanWording.ruleName("maven.target"))
        #expect(CleanWording.ruleName("cmake.build") != CleanWording.ruleName("gradle.build"))
    }

    @Test("the second line says the protection instead of the notes, which cannot tell a flag from Settings")
    func secondLine() throws {
        let items = ReviewItems.make(report: try ScanReport.read(try Fixture.data("scan-report-protection")), settings: nil, rules: try Fixture.rules())
        let rows = try (0..<3).map { try #require(items.row($0)) }
        #expect(CleanWording.detail(rows[1], home: F.home)
            == "Your data · shown, not cleaned · Holds a folder skipped in Settings: ~/q/node_modules/raw · otherwise: node.node_modules")
        #expect(CleanWording.detail(rows[2], home: F.home)
            == "Your data · shown, not cleaned · Excluded by a command-line flag · The next scan without the flag lists this folder")
        // Without `home`, the paths show in full.
        #expect(CleanWording.detail(rows[1]).contains("Settings: /Users/example/q/node_modules/raw"))
        #expect(CleanWording.ownDetail(rows[2], omitting: [], home: F.home) == "Excluded by a command-line flag · The next scan without the flag lists this folder")
        // A report without the field keeps its notes, and `.excluded` names no origin it does not know.
        let old = try Fixture.editedFindings("scan-report-protection") { findings in
            for index in findings.indices { findings[index].removeValue(forKey: "protection") }
        }
        let plain = ReviewItems.make(report: try ScanReport.read(old), settings: nil, rules: try Fixture.rules())
        #expect(CleanWording.detail(try #require(plain.row(2))) == "Your data · shown, not cleaned · Excluded in Settings or by a command-line flag")
        #expect(CleanWording.detail(try #require(plain.row(0))) == "Your data · shown, not cleaned · Protected by .ddc-keep")
        #expect(CleanWording.excluded(.settings) == "Excluded in Settings")
    }

    @Test("Protected by you says what it is at its top, and its heading names no note")
    func blurb() throws {
        let items = ReviewItems.make(report: try ScanReport.read(try Fixture.data("scan-report-protection")), settings: nil, rules: try Fixture.rules())
        let entry = try #require(items.sidebar.flatMap(\.entries).first { $0.group == "Protected by you" })
        let layout = items.layout(entry, matching: "")
        #expect(layout.blurb == CleanWording.keptBlurb)
        #expect(layout.blurb == "Folders you protected with a .ddc-keep file, an exclusion or a skip. DeepDriveCleaner never removes them. Remove the protection to clean one.")
        #expect(layout.facts == "user.kept · Report only")
        let details = layout.blocks.flatMap { block -> [String] in
            if case .subgroup(let subgroup) = block { return subgroup.items.map(\.detail) }
            return []
        }
        #expect(details.contains("Holds a folder skipped in Settings: ~/q/node_modules/raw · otherwise: node.node_modules"))
        // The other user-data groups keep the class's words.
        let pictures = try ReviewFixture.items()
        let other = try #require(pictures.sidebar.flatMap(\.entries).first { $0.group == "Pictures" })
        #expect(pictures.layout(other, matching: "").blurb != CleanWording.keptBlurb)
        // The section's words promise picking only where a folder lists its files.
        #expect(CleanWording.userDataSectionBlurb.contains("Where a folder lists its files"))
    }

    // MARK: - Remove Protection… on a row

    @Test("Remove Protection… is offered for one marker or one Settings entry, never for two, a flag, or a foreign report")
    @MainActor
    func canRemove() async throws {
        let data = try TempDataDirectory()
        let defaults = try TempDefaults()
        let runner = FakeRunner(scan: FakeRunner.done(Data()))
        var report = try Fixture.data("scan-report-protection")
        let model = makeReview(runner: runner, data: data, defaults: defaults) { try? F.snapshot(report) }
        #expect(model.open(ReviewRequest(group: nil, classes: [], confirm: false)))
        #expect(!model.canRemoveProtection(0), "two markers")
        #expect(model.canRemoveProtection(1), "one skip of Settings")
        #expect(!model.canRemoveProtection(2), "a flag's exclusion")
        report = try F.report(1, F.protection(
            markers: [F.marker("/Users/example/q/node_modules", ino: 339_569_720)],
            exclusions: [F.exclusion("/Users/example/q/node_modules/raw", kind: "skip", origin: "settings", inside: true)]
        ))
        model.reload()
        #expect(!model.canRemoveProtection(1), "a marker and a skip")
        report = try F.oneMarker()
        model.reload()
        #expect(model.canRemoveProtection(0), "one marker")
        // Of another home: no Remove Protection…, as no Clean or Protect.
        report = try Fixture.edited("scan-report-protection") { $0["home"] = "/Users/other"; $0["scan_roots"] = ["/Users/other"] }
        model.reload()
        #expect(model.reportScope == .outside)
        #expect(!model.canRemoveProtection(1))
        // Without the CLI a marker cannot go; a Settings entry still can.
        report = try F.oneMarker()
        let alone = makeReview(runner: nil, data: data, defaults: defaults) { try? F.snapshot(report) }
        #expect(alone.open(ReviewRequest(group: nil, classes: [], confirm: false)))
        #expect(!alone.canRemoveProtection(0) && alone.canRemoveProtection(1))
    }

    @Test("a marker goes through one queued keep remove with its identity; removed and absent rescan, refused and failed do not")
    @MainActor
    func removeMarker() async throws {
        let cases: [(CLIRunner.Result, rescans: Int, notice: String?)] = [
            (F.keep("removed"), 1, nil),
            (F.keep("absent"), 1, nil),
            (F.keep("refused", extra: #", "reason": "changed-since-scan""#), 0, "~/p/node_modules changed since the scan; nothing was removed. Scan Again first."),
            (F.keep("refused", extra: #", "reason": "not-a-file""#), 0,
             "The .ddc-keep in ~/p/node_modules is not a file (a symlink or a folder), so it was not removed. Remove it in Finder."),
            (F.keep("failed", extra: #", "error": "Permission denied""#), 0, "The .ddc-keep in ~/p/node_modules could not be removed (Permission denied)."),
            (CLIRunner.Result(outcome: .busy, stdout: Data(), stderrTail: []), 0,
             "Another scan or clean holds the lock, perhaps from Terminal. Try again when it ends."),
        ]
        for (reply, rescans, notice) in cases {
            let data = try TempDataDirectory()
            let defaults = try TempDefaults()
            let events = ReviewEvents()
            let runner = FakeRunner(scan: FakeRunner.done(Data()))
            await runner.reply(.keepRemove, reply)
            let report = try F.oneMarker()
            let model = makeReview(runner: runner, data: data, defaults: defaults, events: events) { try? F.snapshot(report) }
            #expect(model.open(ReviewRequest(group: nil, classes: [], confirm: false)))
            let confirmation = try #require(await model.removalConfirmation(0))
            await model.removeProtection(confirmation)
            #expect(await runner.arguments(of: .keepRemove) == [[
                "keep", "remove", "/Users/example/p/node_modules", "--folder-dev", "16777232", "--folder-ino", "339569697", "--json",
            ]])
            #expect(await runner.count(.scan) == 0, "the rescan is the window's, not a scan of its own")
            #expect(events.rescans == rescans, "\(reply.outcome) \(String(decoding: reply.stdout, as: UTF8.self))")
            // Said on the row, not above the window.
            #expect(model.protectionMessage(for: 0) == notice)
            #expect(model.protectionMessage(for: 1) == nil)
            #expect(model.notice == nil)
            #expect(model.removingProtection == nil && model.rescanning == nil)
            // Until the rows are built again.
            model.reload()
            #expect(model.protectionMessage(for: 0) == nil)
        }
    }

    @Test("a refusal stays on its row by the finding's identity, and a second attempt clears it first")
    @MainActor
    func messageFollowsTheFinding() async throws {
        let data = try TempDataDirectory()
        let defaults = try TempDefaults()
        let events = ReviewEvents()
        events.holdRescan = true
        let runner = FakeRunner(scan: FakeRunner.done(Data()))
        await runner.reply(.keepRemove, F.keep("refused", extra: #", "reason": "not-a-file""#))
        let report = try F.oneMarker()
        let model = makeReview(runner: runner, data: data, defaults: defaults, events: events) { try? F.snapshot(report) }
        #expect(model.open(ReviewRequest(group: nil, classes: [], confirm: false)))
        await model.removeProtection(try #require(await model.removalConfirmation(0)))
        #expect(model.protectionMessage(for: 0)?.hasSuffix("Remove it in Finder.") == true)
        await runner.reply(.keepRemove, F.keep("removed"))
        let again = Task { await model.removeProtection(try #require(await model.removalConfirmation(0))) }
        try await until("the rescan") { model.rescanning == .afterRemovingProtection }
        #expect(model.protectionMessage(for: 0) == nil, "cleared when the row tries again")
        events.finishRescan()
        try await again.value
        #expect(events.rescans == 1)
    }

    @Test("a report of another folder, another version or no report is a failure, and nothing rescans")
    @MainActor
    func unreadableReport() async throws {
        for stdout in [
            #"{"schema_version": 1, "path": "/Users/example/other", "dev": 1, "ino": 2, "rule": null, "result": "removed"}"#,
            #"{"schema_version": 2, "path": "/Users/example/p/node_modules", "dev": 1, "ino": 2, "rule": null, "result": "removed"}"#,
            #"{"schema_version": 1, "path": "/Users/example/p/node_modules", "dev": 1, "ino": 2, "rule": null, "result": "renamed"}"#,
            "",
        ] {
            let data = try TempDataDirectory()
            let defaults = try TempDefaults()
            let events = ReviewEvents()
            let runner = FakeRunner(scan: FakeRunner.done(Data()))
            await runner.reply(.keepRemove, FakeRunner.done(Data(stdout.utf8)))
            let report = try F.oneMarker()
            let model = makeReview(runner: runner, data: data, defaults: defaults, events: events) { try? F.snapshot(report) }
            #expect(model.open(ReviewRequest(group: nil, classes: [], confirm: false)))
            await model.removeProtection(try #require(await model.removalConfirmation(0)))
            #expect(events.rescans == 0, "\(stdout)")
            #expect(model.protectionMessage(for: 0)?.contains("Scan Again") == true, "\(stdout)")
        }
    }

    @Test("while a protection goes and the scan after it runs, nothing else starts from the window; a second press runs nothing")
    @MainActor
    func rowWhileScanning() async throws {
        let data = try TempDataDirectory()
        let defaults = try TempDefaults()
        let events = ReviewEvents()
        events.holdRescan = true
        let runner = HeldKeep(F.keep("removed"))
        // Finding 0 with one marker, and finding 2 a node_modules the window could clean or protect.
        let scan = try #require(try JSONSerialization.jsonObject(with: Fixture.data("scan-report")) as? [String: Any])
        var cleanable = try #require((scan["findings"] as? [[String: Any]])?.first { $0["class"] as? String == "rebuildable" })
        cleanable["path"] = "/Users/example/s/node_modules"
        cleanable["ino"] = 339_569_740
        let report = try Fixture.editedFindings("scan-report-protection") { findings in
            findings[0]["protection"] = F.protection(markers: [F.marker()], rule: "node.node_modules")
            findings[2] = cleanable
        }
        let model = makeReview(runner: runner, data: data, defaults: defaults, events: events) { try? F.snapshot(report) }
        #expect(model.open(ReviewRequest(group: nil, classes: [.rebuildable], confirm: false)))
        #expect(model.canClean(scope: nil) && model.canProtect(2), "before: the other row can be cleaned and protected")
        let confirmation = try #require(await model.removalConfirmation(0))
        let run = Task { await model.removeProtection(confirmation) }

        // keep remove has not answered.
        try await until("the removal") { model.removingProtection?.stage == .removing }
        #expect(model.removingProtection?.text == "Removing the protection…")
        #expect(!model.canClean(scope: nil) && !model.canClean(scope: [2]))
        #expect(!model.canProtect(2) && !model.canRemoveProtection(1))
        model.openSheet()
        #expect(model.sheet == nil)
        await model.scanAgain()
        #expect(events.rescans == 0 && model.rescanning == nil, "Scan Again waits for it")
        await runner.release()

        // The scan after it.
        try await until("the rescan") { model.rescanning == .afterRemovingProtection }
        #expect(model.removingProtection == RemoveProtection.Progress(id: 0, stage: .scanning))
        #expect(model.removingProtection?.text == "Protection removed. Scanning…")
        #expect(!model.canRemoveProtection(1) && !model.canClean(scope: nil) && !model.canProtect(2))
        await model.removeProtection(confirmation)
        #expect(await runner.runs == 1)
        events.finishRescan()
        await run.value
        #expect(model.removingProtection == nil && model.rescanning == nil)
        #expect(events.rescans == 1)
        #expect(model.canClean(scope: nil) && model.canProtect(2), "after: as before")
    }

    @Test("the confirmation names the findings it unprotects and the rule that would claim each")
    @MainActor
    func confirmationWords() async throws {
        let data = try TempDataDirectory()
        let defaults = try TempDefaults()
        let report = try F.oneMarker()
        let model = makeReview(runner: FakeRunner(scan: FakeRunner.done(Data())), data: data, defaults: defaults) { try? F.snapshot(report) }
        #expect(model.open(ReviewRequest(group: nil, classes: [], confirm: false)))
        let marker = try #require(await model.removalConfirmation(0))
        #expect(marker.question == "Remove the .ddc-keep in ~/p/node_modules?")
        #expect(marker.findings == ["~/p/node_modules · otherwise: node.node_modules"])
        #expect(marker.message.contains("in its own group, where it may be selected for cleanup by default"))
        #expect(marker.confirmTitle == "Remove Protection")
        let skip = try #require(await model.removalConfirmation(1))
        #expect(skip.question == "Stop skipping ~/q/node_modules/raw?")
        #expect(skip.message.hasPrefix("This removes ~/q/node_modules/raw from Settings → Folders"))
        #expect(skip.gitWarning == nil)
        #expect(await model.removalConfirmation(2) == nil, "a flag's: no confirmation")
    }

    // MARK: - The Settings route

    @Test("the Settings edit removes every entry, in both lists, whose realpath is the reported path, then rescans")
    @MainActor
    func settingsThroughSymlink() async throws {
        let dir = FileManager.default.temporaryDirectory.appending(path: "ddckit-protect-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: dir) }
        try FileManager.default.createDirectory(at: dir.appending(path: "real/raw"), withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: dir.appending(path: "real/other"), withIntermediateDirectories: true)
        try FileManager.default.createSymbolicLink(at: dir.appending(path: "link"), withDestinationURL: dir.appending(path: "real"))
        try FileManager.default.createSymbolicLink(at: dir.appending(path: "alias"), withDestinationURL: dir.appending(path: "real"))
        let written = dir.appending(path: "link/raw").path
        let canonical = ReportScope.canonical(dir.appending(path: "real/raw").path)
        let other = dir.appending(path: "link/other").path
        #expect(written != canonical)
        var file = SettingsFile()
        // Three spellings of one folder, two of them skips and one an exclude.
        file.skips = [written, canonical, other]
        file.excludes = ["/Users/example/elsewhere", dir.appending(path: "alias/raw").path]
        #expect(FolderList.entries(for: canonical, in: file) == [dir.appending(path: "alias/raw").path, written, canonical])
        var copy = file
        #expect(FolderList.removeEntries(for: canonical, from: &copy).count == 3)
        #expect(copy.skips == [other] && copy.excludes == ["/Users/example/elsewhere"])

        let data = try TempDataDirectory()
        let defaults = try TempDefaults()
        let events = ReviewEvents()
        let report = try F.report(1, F.protection(exclusions: [F.exclusion(canonical, kind: "skip", origin: "settings", inside: true)], rule: "node.node_modules"))
        let model = ReviewModel(
            runner: nil,
            data: data.directory,
            preferences: AppPreferences(defaults: defaults.defaults),
            snapshot: { try? F.snapshot(report) },
            present: { true },
            rescan: { await events.rescan() },
            home: F.home,
            editSettings: { edit in
                edit(&file)
                return true
            }
        )
        #expect(model.open(ReviewRequest(group: nil, classes: [], confirm: false)))
        let confirmation = try #require(await model.removalConfirmation(1))
        await model.removeProtection(confirmation)
        #expect(file.skips == [other])
        #expect(file.excludes == ["/Users/example/elsewhere"], "nothing else changes")
        #expect(model.protectionMessage(for: 1) == nil)
        #expect(events.rescans == 1)
        // An entry already gone is gone: the scan after says so.
        await model.removeProtection(try #require(await model.removalConfirmation(1)))
        #expect(events.rescans == 2)
    }

    @Test("a settings.json that cannot be written keeps the protection, says so, and nothing rescans")
    @MainActor
    func settingsUnwritable() async throws {
        let data = try TempDataDirectory()
        let defaults = try TempDefaults()
        let events = ReviewEvents()
        let model = ReviewModel(
            runner: nil,
            data: data.directory,
            preferences: AppPreferences(defaults: defaults.defaults),
            snapshot: { try? F.snapshot() },
            present: { true },
            rescan: { await events.rescan() },
            home: F.home
        )
        #expect(model.open(ReviewRequest(group: nil, classes: [], confirm: false)))
        await model.removeProtection(try #require(await model.removalConfirmation(1)))
        #expect(events.rescans == 0)
        #expect(model.protectionMessage(for: 1)?.hasPrefix("settings.json could not be changed") == true)
        #expect(model.protectionMessage(for: 0) == nil && model.notice == nil)
    }

    @Test("a model given no git check asks nothing: the default runs no git")
    @MainActor
    func gitOffByDefault() async throws {
        #expect(GitTracking.off.binary == nil)
        let repo = try Repository()
        #expect(await GitTracking.off.marker(in: repo.folder, roots: repo.roots) == .unknown)
        let data = try TempDataDirectory()
        let defaults = try TempDefaults()
        let report = try F.oneMarker()
        let model = makeReview(runner: FakeRunner(scan: FakeRunner.done(Data())), data: data, defaults: defaults) { try? F.snapshot(report) }
        #expect(model.open(ReviewRequest(group: nil, classes: [], confirm: false)))
        #expect(model.gitTracking.binary == nil)
        #expect(try #require(await model.removalConfirmation(0)).gitWarning == nil)
    }

    // MARK: - git

    /// A folder in a repository within its scan root, and executable files
    /// standing for the git binaries; none of them is ever run.
    final class Repository {
        let root: URL
        let folder: String
        let roots: [String]

        init(git: Bool = true) throws {
            root = FileManager.default.temporaryDirectory.appending(path: "ddckit-git-\(UUID().uuidString)")
            try FileManager.default.createDirectory(at: root.appending(path: "home/project/node_modules"), withIntermediateDirectories: true)
            if git { try FileManager.default.createDirectory(at: root.appending(path: "home/project/.git"), withIntermediateDirectories: true) }
            folder = ReportScope.canonical(root.appending(path: "home/project/node_modules").path)
            roots = [ReportScope.canonical(root.appending(path: "home").path)]
        }

        /// An executable file at `name`: present, never run.
        func binary(_ name: String) throws -> String {
            let path = root.appending(path: name).path
            #expect(FileManager.default.createFile(atPath: path, contents: Data(), attributes: [.posixPermissions: 0o755]))
            return path
        }

        deinit {
            try? FileManager.default.removeItem(at: root)
        }
    }

    /// The runs a fake git received, and what it answers.
    final class Runs: Sendable {
        let lines = Lines()
        let status: Int32?

        init(_ status: Int32?) {
            self.status = status
        }

        var run: GitTracking.Run {
            { [lines, status] program, arguments, timeout in
                lines.append(([program] + arguments + ["\(timeout)"]).joined(separator: " "))
                return status
            }
        }
    }

    @Test("git's answer: tracked warns; untracked, a failure or a timeout does not")
    func gitAnswers() async throws {
        let repo = try Repository()
        let missing = repo.root.appending(path: "no-git").path
        let git = try repo.binary("git")
        for (status, answer) in [(Int32(0), GitTracking.Answer.tracked), (1, .untracked), (128, .unknown), (nil, .unknown)] as [(Int32?, GitTracking.Answer)] {
            let runs = Runs(status)
            let tracking = GitTracking(binaries: [missing, git], run: runs.run)
            #expect(await tracking.marker(in: repo.folder, roots: repo.roots) == answer)
            #expect(runs.lines.all == [
                "\(git) -C \(repo.folder) --no-optional-locks -c core.fsmonitor=false ls-files --error-unmatch .ddc-keep \(GitTracking.timeout)",
            ], "the first binary that exists, once, with the timeout")
        }
        #expect(GitTracking.timeout == .seconds(2))
    }

    @Test("with no git binary, or no .git above the marker within its root, nothing is executed")
    func gitNothingRuns() async throws {
        let repo = try Repository()
        let runs = Runs(0)
        let none = GitTracking(binaries: [repo.root.appending(path: "a").path, repo.root.appending(path: "b").path], run: runs.run)
        #expect(none.binary == nil)
        #expect(await none.marker(in: repo.folder, roots: repo.roots) == .unknown)
        let bare = try Repository(git: false)
        let some = GitTracking(binaries: [try bare.binary("git")], run: runs.run)
        #expect(await some.marker(in: bare.folder, roots: bare.roots) == .unknown)
        #expect(await some.marker(in: "/elsewhere/x", roots: bare.roots) == .unknown, "outside every root")
        #expect(runs.lines.all.isEmpty)
    }

    @Test("only four fixed paths, never /usr/bin/git, xcrun or xcode-select")
    func gitBinaries() throws {
        #expect(GitTracking.binaries == [
            "/Library/Developer/CommandLineTools/usr/bin/git",
            "/Applications/Xcode.app/Contents/Developer/usr/bin/git",
            "/opt/homebrew/bin/git",
            "/usr/local/bin/git",
        ])
        let source = try rustSource("app/Packages/DDCKit/Sources/DDCKit/GitTracking.swift")
            .split(separator: "\n").filter { !$0.trimmingCharacters(in: .whitespaces).hasPrefix("//") }.joined(separator: "\n")
        for forbidden in [#""/usr/bin/git""#, "xcrun", "xcode-select", "/usr/bin/env"] {
            #expect(!source.contains(forbidden), "\(forbidden)")
        }
    }

    @Test("the confirmation warns when git tracks the marker, and only then")
    @MainActor
    func gitWarning() async throws {
        let repo = try Repository()
        for (status, warns) in [(Int32(0), true), (1, false), (128, false)] {
            let runs = Runs(status)
            let tracking = GitTracking(binaries: [try repo.binary("git-\(status)")], run: runs.run)
            let report = try Fixture.edited("scan-report-protection") { json in
                json["home"] = repo.roots[0]
                json["scan_roots"] = repo.roots
                var findings = json["findings"] as? [[String: Any]] ?? []
                findings[0]["path"] = repo.folder
                findings[0]["protection"] = F.protection(markers: [F.marker(repo.folder)], rule: "node.node_modules")
                json["findings"] = findings
            }
            let data = try TempDataDirectory()
            let defaults = try TempDefaults()
            let model = ReviewModel(
                runner: FakeRunner(scan: FakeRunner.done(Data())),
                data: data.directory,
                preferences: AppPreferences(defaults: defaults.defaults),
                snapshot: { try? F.snapshot(report) },
                present: { true },
                home: repo.roots[0],
                gitTracking: tracking
            )
            #expect(model.open(ReviewRequest(group: nil, classes: [], confirm: false)))
            let confirmation = try #require(await model.removalConfirmation(0))
            #expect(confirmation.gitWarning == (warns ? RemoveProtection.gitTracked : nil), "\(status)")
            #expect(runs.lines.all.count == 1)
            // A Settings entry has no marker for git to track.
            #expect(await model.removalConfirmation(1)?.gitWarning == nil)
            #expect(runs.lines.all.count == 1)
        }
    }

    @Test("the app's runner returns the status, and nil past the timeout")
    func launch() async {
        #expect(await GitTracking.launch("/bin/sh", ["-c", "exit 1"], .seconds(5)) == 1)
        #expect(await GitTracking.launch("/bin/sh", ["-c", "exit 0"], .seconds(5)) == 0)
        let start = ContinuousClock.now
        #expect(await GitTracking.launch("/bin/sh", ["-c", "sleep 30"], .milliseconds(200)) == nil)
        #expect(ContinuousClock.now - start < .seconds(10))
        #expect(await GitTracking.launch("/nonexistent/git", [], .seconds(1)) == nil)
        // One that ignores SIGTERM is killed `killAfter` later.
        let stubborn = ContinuousClock.now
        #expect(await GitTracking.launch("/bin/sh", ["-c", "trap '' TERM; exec sleep 30"], .milliseconds(200)) == nil)
        #expect(ContinuousClock.now - stubborn < .seconds(10))
        #expect(GitTracking.killAfter == .milliseconds(500))
    }
}
