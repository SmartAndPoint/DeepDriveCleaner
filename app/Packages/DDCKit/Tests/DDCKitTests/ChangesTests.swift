import Foundation
import Testing
@testable import DDCKit

/// What grew between the last two scans, in the popover (story "Popover and
/// Review: what grew since the earlier scan", DDC-002). The fixtures are what
/// the CLI's `compare` writes (`the_apps_changes_fixtures_are_reports_the_cli_wrote`).
@Suite("What grew since the earlier scan", .timeLimit(.minutes(1)))
@MainActor
struct ChangesTests {
    let data: TempDataDirectory
    let defaults: TempDefaults

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
    }

    /// The fixture's `current.written_at`, and a time the next morning.
    static let written = "2026-10-19T08:05:00Z"
    static let morning = Date(timeIntervalSince1970: 1_792_404_000)  // 2026-10-19T10:00:00Z
    static let utc: Calendar = {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return calendar
    }()
    static let en = Locale(identifier: "en_GB")
    static let roots = ["/Users/example"]
    static let G: Int64 = 1 << 30

    private func changes(_ edit: (inout [String: Any]) -> Void = { _ in }) throws -> ChangesReport {
        try ChangesReport.read(Fixture.edited("changes-compared", edit))
    }

    private func attributed(_ report: ChangesReport, roots: [String] = Self.roots) -> GrowthBand? {
        GrowthBand.attributed(report, roots: roots, home: "/Users/example", now: Self.morning, calendar: Self.utc, locale: Self.en)
    }

    /// Where the 60 GiB went, set by hand: `rest`, `elsewhere` and the groups.
    private static func split(rest: Int64, elsewhere: Int64, groups: [[String: Any]], compared: Bool = true) -> (inout [String: Any]) -> Void {
        { json in
            var disk = json["disk"] as! [String: Any]
            disk["delta"] = 60 * G
            json["disk"] = disk
            var restJSON = json["rest"] as! [String: Any]
            restJSON["delta"] = rest
            json["rest"] = restJSON
            var home = json["home"] as! [String: Any]
            home["compared"] = compared
            home["elsewhere"] = compared ? elsewhere : NSNull()
            home["delta"] = compared ? 60 * G - rest : NSNull()
            home["reasons"] = compared ? [] : ["different-access"]
            json["home"] = home
            json["groups"] = groups
        }
    }

    private static func group(_ label: String, _ delta: Int64, new: Int = 0, grew: Int = 0, harness: [[String: Any]] = []) -> [String: Any] {
        ["group": label, "delta": delta, "new": new, "grew": grew, "shrank": 0, "gone": 0, "cleaned": 0, "moved": 0, "by_harness": harness]
    }

    // MARK: - The result

    @Test("the CLI's fixtures decode: compared with grew, and not compared with its reason")
    func decodes() throws {
        let compared = try ChangesReport.read(Fixture.data("changes-compared"))
        #expect(compared.status == .compared && compared.grew)
        #expect(compared.current?.writtenAt == Self.written)
        #expect(compared.baseline?.byClean == false)
        #expect(compared.groups.map(\.group) == ["Build outputs", "node_modules"])
        #expect(compared.items.map(\.kind) == [.new, .grew])
        #expect(compared.home?.compared == true)
        let not = try ChangesReport.read(Fixture.data("changes-not-compared"))
        #expect(not.status == .notCompared && not.reason == .noPrevious && !not.grew)
        #expect(not.disk == nil && not.home == nil && not.items.isEmpty)
    }

    @Test("strict: an unknown kind, another schema version or a missing grew is refused; an unknown key is skipped")
    func strict() throws {
        let refusals: [(inout [String: Any]) -> Void] = [
            { json in
                var items = json["items"] as! [[String: Any]]
                items[0]["kind"] = "exploded"
                json["items"] = items
            },
            { $0["status"] = "maybe" },
            { $0.removeValue(forKey: "grew") },
            { $0.removeValue(forKey: "disk") },
        ]
        for edit in refusals {
            #expect(throws: (any Error).self) { try ChangesReport.read(Fixture.edited("changes-compared", edit)) }
        }
        #expect(throws: ScanReport.ReadError.otherSchema(2)) {
            try ChangesReport.read(Fixture.edited("changes-compared") { $0["schema_version"] = 2 })
        }
        _ = try changes { $0["something_new"] = true }
    }

    // MARK: - The band's lines

    @Test("the first line says how much since when; a clean's baseline says after your clean")
    func firstLine() throws {
        let band = try #require(attributed(try changes()))
        #expect(band.text.hasPrefix("↑ 60.0 GiB more in use since yesterday "))
        #expect(band.scanButton == .none && !band.warning)
        let clean = try #require(attributed(try changes { json in
            var baseline = json["baseline"] as! [String: Any]
            baseline["written_by"] = "clean-items"
            json["baseline"] = baseline
        }))
        #expect(clean.text.hasPrefix("↑ 60.0 GiB more in use than after your clean yesterday "))
        let today = try #require(attributed(try changes { json in
            var baseline = json["baseline"] as! [String: Any]
            baseline["written_by"] = "clean"
            baseline["written_at"] = "2026-10-19T08:00:00Z"
            json["baseline"] = baseline
        }))
        #expect(today.text.hasPrefix("↑ 60.0 GiB more in use than after your clean at "), "\(today.text)")
    }

    @Test("no band without grew, on a shrink, or when not compared")
    func noBand() throws {
        #expect(attributed(try changes { $0["grew"] = false }) == nil)
        #expect(attributed(try changes { json in
            var disk = json["disk"] as! [String: Any]
            disk["delta"] = -5 * Self.G
            json["disk"] = disk
        }) == nil)
        #expect(attributed(try ChangesReport.read(Fixture.data("changes-not-compared"))) == nil)
    }

    @Test("mostly not read or outside the home: says so and links to Where Did It Go?; with the home not compared, outside both scans' folders")
    func mostlyOutside() throws {
        // The fixture: the findings explain half a MiB of 60 GiB.
        let band = try #require(attributed(try changes()))
        #expect(band.detail == "Most of it is in folders ddc did not read or outside your home folder")
        #expect(band.link == .whereDidItGo)
        let notCompared = try #require(attributed(try changes(Self.split(rest: 40 * Self.G, elsewhere: 0, groups: [Self.group("Build outputs", 20 * Self.G)], compared: false))))
        #expect(notCompared.detail == "Most of it is outside the folders both scans read")
        // No group grew at all: the same line.
        let none = try #require(attributed(try changes(Self.split(rest: 10 * Self.G, elsewhere: 20 * Self.G, groups: []))))
        #expect(none.link == .whereDidItGo)
    }

    @Test("mostly elsewhere in the home folder, in no finding")
    func mostlyElsewhere() throws {
        let band = try #require(attributed(try changes(Self.split(rest: 5 * Self.G, elsewhere: 40 * Self.G, groups: [Self.group("Build outputs", 15 * Self.G, new: 2)]))))
        #expect(band.detail == "Most of it is elsewhere in your home folder, in folders ddc does not clean")
        #expect(band.link == .seeWhatGrew)
        // 50 GiB of videos in ~/Movies, no group grew: still the home folder.
        let videos = try #require(attributed(try changes(Self.split(rest: 5 * Self.G, elsewhere: 50 * Self.G, groups: []))))
        #expect(videos.detail == "Most of it is elsewhere in your home folder, in folders ddc does not clean")
    }

    @Test("one group at 60 % or more is named with its new count and its coding agent; else the number of groups")
    func groups() throws {
        let claude: [[String: Any]] = [["harness": "claude-code", "delta": 40 * Self.G, "count": 9]]
        let one = try #require(attributed(try changes(Self.split(rest: 2 * Self.G, elsewhere: 3 * Self.G, groups: [
            Self.group("Build outputs", 50 * Self.G, new: 14, grew: 2, harness: claude),
            Self.group("node_modules", 5 * Self.G, grew: 1),
        ]))))
        #expect(one.detail == "Build outputs +50.0 GiB · 14 new · mostly in Claude Code worktrees or data")
        #expect(one.link == .seeWhatGrew)
        let grown = try #require(attributed(try changes(Self.split(rest: 2 * Self.G, elsewhere: 3 * Self.G, groups: [Self.group("Xcode builds", 55 * Self.G, grew: 3)]))))
        #expect(grown.detail == "Xcode builds +55.0 GiB · 3 grew")
        let even = try #require(attributed(try changes(Self.split(rest: 2 * Self.G, elsewhere: 3 * Self.G, groups: [
            Self.group("Build outputs", 30 * Self.G, new: 1),
            Self.group("node_modules", 25 * Self.G, new: 1),
            Self.group("Package downloads", -4 * Self.G),
        ]))))
        #expect(even.detail == "in 2 groups")
    }

    @Test("the person's cleans, at a quarter of the groups' growth, and narrower compared folders are said on any line")
    func clauses() throws {
        let cleaned: (inout [String: Any]) -> Void = { json in
            Self.split(rest: 2 * Self.G, elsewhere: 3 * Self.G, groups: [Self.group("Build outputs", 55 * Self.G, new: 4)])(&json)
            json["cleaned_since"] = ["deleted": 10 * Self.G, "trashed": 4 * Self.G]
        }
        #expect(attributed(try changes(cleaned))?.detail == "Build outputs +55.0 GiB · 4 new · you cleaned 14.0 GiB since")
        let small: (inout [String: Any]) -> Void = { json in
            cleaned(&json)
            json["cleaned_since"] = ["deleted": 10 * Self.G, "trashed": 3 * Self.G]
        }
        #expect(attributed(try changes(small))?.detail == "Build outputs +55.0 GiB · 4 new", "under a quarter")
        let project = try #require(attributed(try changes { $0["compared_roots"] = ["/Users/example/p1"] }))
        #expect(project.detail == "Most of it is in folders ddc did not read or outside your home folder · compared in ~/p1 only")
    }

    @Test("since: today, yesterday, a weekday within the week, else the date")
    func since() throws {
        let at = { (text: String) in try #require(Wording.date(rfc3339: text)) }
        let since = { (date: Date) in Wording.since(date, now: Self.morning, calendar: Self.utc, locale: Self.en) }
        #expect(since(try at("2026-10-19T08:05:00Z")).hasPrefix("today "))
        #expect(since(try at("2026-10-18T23:59:00Z")).hasPrefix("yesterday "))
        let weekday = since(try at("2026-10-14T07:00:00Z"))
        #expect(weekday.hasPrefix("Wednesday "), "\(weekday)")
        let older = since(try at("2026-10-12T07:00:00Z"))
        #expect(older.contains("Oct") && older.contains("12") && older.contains(", "), "\(older)")
    }

    // MARK: - The model

    /// The groups report as the CLI saved it at `written`, or without the field.
    private func writeReport(written: String? = Self.written, generatedAt: String? = nil, readAt: String? = nil) throws {
        try data.write(lastScan: try report(written: written, generatedAt: generatedAt, readAt: readAt))
    }

    private func report(written: String? = Self.written, generatedAt: String? = nil, readAt: String? = nil) throws -> Data {
        try Fixture.edited("scan-report-groups") { json in
            if let written { json["written_at"] = written }
            if let generatedAt { json["generated_at"] = generatedAt }
            if let readAt, var volumes = json["volumes"] as? [[String: Any]] {
                volumes[0]["read_at"] = readAt
                json["volumes"] = volumes
            }
        }
    }

    /// The compared fixture as the result for a report saved at `written`.
    private func result(for written: String) throws -> Data {
        try Fixture.edited("changes-compared") { json in
            var current = json["current"] as! [String: Any]
            current["written_at"] = written
            json["current"] = current
        }
    }

    private func model(_ runner: any CLIRunning, router: FakeRouter = FakeRouter()) throws -> PopoverModel {
        try makeModel(runner: runner, data: data, defaults: defaults, router: router, clock: { Self.morning })
    }

    @Test("the model asks once per loaded report and shows the band only for it")
    func model() async throws {
        try writeReport()
        let runner = HoldingRunner()
        await runner.reply(.changes, try Fixture.data("changes-compared"))
        let model = try model(runner)
        await model.start()
        #expect(await runner.count(.changes) == 1)
        #expect(model.attributedChanges != nil)
        let band = try #require(model.growthBand)
        #expect(band.link == .whereDidItGo && band.scanButton == .none)
        await model.refresh()
        #expect(await runner.count(.changes) == 1, "once per report")
        #expect(!model.growthPending, "the attributed band lights no badge")
    }

    @Test("a result of another report, or a report from before 0.31, shows nothing")
    func otherReport() async throws {
        try writeReport(written: "2026-10-19T09:00:00Z")
        let runner = HoldingRunner()
        await runner.reply(.changes, try Fixture.data("changes-compared"))
        let model = try model(runner)
        await model.start()
        #expect(model.changes != nil && model.attributedChanges == nil && model.growthBand == nil)

        try writeReport(written: nil)
        let old = HoldingRunner()
        let earlier = try self.model(old)
        await earlier.start()
        #expect(await old.count(.changes) == 0, "no written_at: nothing to compare against")
        #expect(earlier.growthBand == nil)
    }

    @Test("the signal band wins over the attributed one; once it is quiet the attributed band shows")
    func precedence() async throws {
        // The space fixture's baseline is a scan at this time.
        try writeReport(generatedAt: "2026-11-02T08:00:00Z")
        let runner = HoldingRunner()
        await runner.reply(.changes, try Fixture.data("changes-compared"))
        await runner.reply(.space, try Fixture.data("space-grew"))
        let model = try model(runner)
        await model.start()
        await model.popoverOpened()
        let signal = try #require(model.growthBand)
        #expect(signal.link == nil && signal.scanButton == .enabled)
        await runner.reply(.space, try Fixture.data("space-quiet"))
        await model.popoverOpened()
        #expect(model.growthBand?.link == .whereDidItGo)
    }

    @Test("the app's own scan asks again and shows the band for its report; none while it runs")
    func ownScan() async throws {
        try writeReport()
        let later = "2026-10-19T09:40:00Z"
        let runner = HoldingRunner(holding: [.scan])
        await runner.reply(.changes, try Fixture.data("changes-compared"))
        await runner.reply(.scan, try report(written: later))
        let model = try model(runner)
        await model.start()
        #expect(model.growthBand != nil)
        await runner.reply(.changes, try result(for: later))
        let scan = Task { await model.scanNow() }
        try await runner.waitHeld()
        #expect(model.growthBand == nil, "not during a scan")
        await runner.release()
        await scan.value
        #expect(await runner.count(.changes) == 2)
        #expect(model.attributedChanges?.current?.writtenAt == later)
        #expect(model.growthBand?.link == .whereDidItGo)
    }

    @Test("low disk wins over the attributed band; paused keeps its text")
    func lowDiskAndPaused() async throws {
        // The low-disk fixture's baseline is a clean's reading at this time.
        try writeReport(readAt: "2026-11-03T07:15:00Z")
        let runner = HoldingRunner()
        await runner.reply(.changes, try Fixture.data("changes-compared"))
        let model = try model(runner)
        await model.start()
        let band = try #require(model.growthBand)
        model.setPaused(true)
        #expect(model.growthBand == band, "paused: the same text and link")
        model.setPaused(false)
        await runner.reply(.space, try Fixture.data("space-low-disk"))
        await model.popoverOpened()
        let low = try #require(model.growthBand)
        #expect(low.warning && low.link == nil)
    }

    @Test("an attributed band on screen does not use up the signal's badge")
    func badge() async throws {
        // The report's own baseline is not the space result's: no signal band.
        try writeReport()
        let runner = HoldingRunner()
        await runner.reply(.changes, try Fixture.data("changes-compared"))
        await runner.reply(.space, try Fixture.data("space-grew"))
        let model = try model(runner)
        await model.start()
        await model.popoverOpened()
        #expect(model.growthBand?.link != nil, "the attributed band shows")
        #expect(model.growthPending, "the signal is for another baseline: badged")
    }

    @Test("See What Grew asks the Review window for the entry, keeping the person's ticks")
    func link() async throws {
        try writeReport()
        let runner = HoldingRunner()
        await runner.reply(.changes, try Fixture.data("changes-compared"))
        let router = FakeRouter()
        let model = try model(runner, router: router)
        await model.start()
        model.seeWhatGrew(.notRead)
        let request = try #require(router.requests.last)
        #expect(request.grew == .notRead && request.keepsSelection && !request.confirm && request.group == nil)
    }

    @Test("a grew request on an open window keeps what the person ticked; on a closed one it opens with the default")
    func reviewKeepsTicks() throws {
        let snapshot = ReviewSnapshot(report: try ScanReport.read(Fixture.data("scan-report-groups")), rules: try Fixture.rules())
        let review = makeReview(runner: nil, data: data, defaults: defaults) { snapshot }
        let request = ReviewRequest(group: nil, classes: [.safe, .rebuildable], confirm: false, keepsSelection: true, grew: .top)
        #expect(review.open(request))
        let items = try #require(review.items)
        #expect(review.selection == items.defaultSelection(classes: [.safe, .rebuildable]))
        let modules = try ReviewFixture.id(items, "p1/node_modules")
        review.toggle(modules)
        let ticked = review.selection
        #expect(review.open(request))
        #expect(review.selection == ticked)
        review.windowClosed()
        #expect(review.open(request))
        #expect(review.selection == items.defaultSelection(classes: [.safe, .rebuildable]))
    }
}
