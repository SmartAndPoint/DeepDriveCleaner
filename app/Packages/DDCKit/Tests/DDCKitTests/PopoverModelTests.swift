import Foundation
import Testing
@testable import DDCKit

@Suite("Popover model", .timeLimit(.minutes(1)))
@MainActor
struct PopoverModelTests {
    let data: TempDataDirectory
    let defaults: TempDefaults

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
    }

    // MARK: Scan now

    @Test("pressing Scan now twice while a scan runs asks the runner once and shows the running scan")
    func scanNowJoins() async throws {
        let started = Date(timeIntervalSince1970: 1_800_000_000)
        let runner = FakeRunner(scan: FakeRunner.done(try Fixture.data("scan-report-groups")), blocks: true)
        let model = try makeModel(runner: runner, data: data, defaults: defaults, clock: { started })
        await model.start()
        #expect(model.content == .firstScan)

        let first = Task { await model.scanNow() }
        try await runner.waitForScan()
        let second = Task { await model.scanNow() }
        await Task.yield()

        #expect(model.content == .scanning)
        #expect(model.subtitle == "Scan in progress")
        #expect(model.scan?.started == started)
        #expect(model.iconState.inner == .arc)
        #expect(Wording.elapsed(started.addingTimeInterval(17).timeIntervalSince(started)) == "17 s")

        await runner.finishScan()
        await first.value
        await second.value
        #expect(await runner.count(.scan) == 1)
        #expect(model.scan == nil)
        #expect(model.loadedReport?.findings.count == 11, "the scan's report is shown")
        #expect(model.iconState.inner != .arc)
    }

    @Test("the report on disk is read at start and again when the file changes")
    func readsTheFile() async throws {
        try data.write(lastScan: try Fixture.data("scan-report"))
        try data.write(settings: #"{"schema_version": 1}"#)
        let model = try makeModel(runner: nil, data: data, defaults: defaults)
        await model.start()
        #expect(model.loadedReport?.findings.count == 3)

        try data.write(lastScan: try Fixture.data("scan-report-groups"))
        try FileManager.default.setAttributes([.modificationDate: Date.now.addingTimeInterval(60)], ofItemAtPath: data.directory.lastScan.path)
        await model.refresh()
        #expect(model.loadedReport?.findings.count == 11)
    }

    @Test("a report with nothing to free shows Nothing found to clean and no biggest finds")
    func nothingFound() async throws {
        try data.write(lastScan: try Fixture.edited("scan-report-groups") { $0["findings"] = [[String: Any]]() })
        let runner = FakeRunner(scan: FakeRunner.done(Data()), rules: FakeRunner.done(try Fixture.data("rules")))
        let model = try makeModel(runner: runner, data: data, defaults: defaults)
        await model.start()
        #expect(model.content == .nothingFound)
        #expect(model.summary?.topGroups.isEmpty == true)
    }

    @Test("under the threshold the popover says nothing is worth cleaning; over it, it shows the finds")
    func threshold() async throws {
        try data.write(lastScan: try Fixture.data("scan-report-groups"))
        try data.write(settings: #"{"schema_version": 1, "disabled_rules": ["cache.pip"], "min_size": 65536}"#)
        let runner = FakeRunner(scan: FakeRunner.done(Data()), rules: FakeRunner.done(try Fixture.data("rules")))
        let model = try makeModel(runner: runner, data: data, defaults: defaults)
        await model.start()
        #expect(model.content == .nothingWorthCleaning, "a few MiB against the 10 GiB default")

        try data.write(settings: try String(contentsOf: Fixture.url("scan-report-groups.settings"), encoding: .utf8))
        try FileManager.default.setAttributes([.modificationDate: Date.now.addingTimeInterval(60)], ofItemAtPath: data.directory.settings.path)
        await model.refresh()
        #expect(model.content == .found, "over the 1 MiB threshold of the scan-time settings")
        #expect(model.summary?.topGroups.count == 4)
        #expect(model.scope == "in your home folder")
    }

    // MARK: Failures

    @Test("a scan that exits 1 shows the CLI's last ddc line and keeps the last report")
    func scanError() async throws {
        try data.write(lastScan: try Fixture.data("scan-report"))
        let message = "none of the folders to scan in /Users/example/Library/Application Support/DeepDriveCleaner/settings.json exists: /Volumes/Work"
        let failed = CLIRunner.Result(
            outcome: .error(status: 1),
            stdout: Data(),
            stderrTail: ["ddc: warning: cannot raise the open-file limit: x", "ddc: \(message)"]
        )
        let model = try makeModel(runner: FakeRunner(scan: failed), data: data, defaults: defaults)
        await model.start()
        await model.scanNow()
        #expect(model.notices.map(\.text) == [message])
        #expect(model.loadedReport?.findings.count == 3, "the last report stays")
    }

    @Test("a busy lock says a clean is running")
    func busy() async throws {
        let model = try makeModel(
            runner: FakeRunner(scan: CLIRunner.Result(outcome: .busy, stdout: Data(), stderrTail: [])),
            data: data,
            defaults: defaults
        )
        await model.start()
        await model.scanNow()
        #expect(model.notices.map(\.text) == ["A clean is running; scan again when it ends."])
    }

    @Test("a report of another schema version could not be read, and Scan now stays")
    func otherSchema() async throws {
        try data.write(lastScan: try Fixture.data("scan-report-v2"))
        let model = try makeModel(runner: FakeRunner(scan: FakeRunner.done(Data())), data: data, defaults: defaults)
        await model.start()
        #expect(model.content == .unreadable("It was written by another version of ddc (schema 2)."))
        #expect(model.canScan)
    }

    @Test("with no embedded CLI the popover says so, cannot scan, and still shows the last report")
    func noCLI() async throws {
        try data.write(lastScan: try Fixture.data("scan-report"))
        let model = try makeModel(runner: nil, data: data, defaults: defaults)
        await model.start()
        #expect(!model.canScan)
        #expect(model.notices.map(\.id) == ["cli"])
        #expect(model.loadedReport != nil)
        await model.scanNow()
        #expect(model.notices.count == 1)
    }

    @Test("a malformed settings.json is a notice, and the scan's own selection is shown")
    func malformedSettings() async throws {
        try data.write(lastScan: try Fixture.data("scan-report-groups"))
        try data.write(settings: #"{"schema_version": 1, "roots": "~"}"#)
        let model = try makeModel(runner: nil, data: data, defaults: defaults)
        await model.start()
        #expect(model.notices.contains { $0.id == "settings" })
        let report = try #require(model.loadedReport)
        #expect(model.summary?.reclaimable == report.totals.selectedReclaimableBytes)
    }

    // MARK: Full Disk Access

    @Test("Full Disk Access follows the probe, not the report")
    func accessFollowsTheProbe() async throws {
        try data.write(lastScan: try Fixture.edited("scan-report-groups") { $0["full_disk_access"] = true })
        let missing = try makeModel(runner: nil, data: data, defaults: defaults, probeGranted: false)
        await missing.start()
        #expect(!missing.fullDiskAccess.isGranted)
        #expect(missing.iconState.mark == .alert)
        #expect(missing.withoutAccessNote == nil, "the report itself had access")

        try data.write(lastScan: try Fixture.data("scan-report-groups"))
        let granted = try makeModel(runner: nil, data: data, defaults: defaults, probeGranted: true)
        await granted.start()
        #expect(granted.fullDiskAccess.isGranted)
        #expect(granted.iconState.mark != .alert)
        #expect(granted.withoutAccessNote == "This scan ran without Full Disk Access; part of ~/Library was not read.")
        #expect(granted.content == .nothingWorthCleaning, "the note shows in this state too")
    }

    @Test("the probe reports granted for a readable file and missing for a missing or unreadable one")
    func probe() throws {
        let readable = data.url.appending(path: "readable")
        try Data("x".utf8).write(to: readable)
        #expect(FullDiskAccessProbe(path: readable).check() == .granted)
        #expect(FullDiskAccessProbe(path: data.url.appending(path: "absent")).check() == .missing(errno: ENOENT))
        let locked = data.url.appending(path: "locked")
        try Data("x".utf8).write(to: locked)
        try FileManager.default.setAttributes([.posixPermissions: 0], ofItemAtPath: locked.path)
        defer { try? FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: locked.path) }
        #expect(FullDiskAccessProbe(path: locked).check() == .missing(errno: EACCES))
    }

    // MARK: The review seam and the badge

    @Test("Review…, a find and Clean selected… hand the seam the group and the classes switched on")
    func reviewSeam() async throws {
        let router = FakeRouter()
        let model = try makeModel(runner: nil, data: data, defaults: defaults, router: router)
        model.review()
        model.review(group: "node_modules")
        model.includeSafe = false
        model.cleanSelected()
        #expect(router.requests == [
            ReviewRequest(group: nil, classes: [.safe, .rebuildable], confirm: false),
            ReviewRequest(group: "node_modules", classes: [.safe, .rebuildable], confirm: false),
            ReviewRequest(group: nil, classes: [.rebuildable], confirm: true),
        ])
        #expect(model.notices.map(\.text) == ["The Review window arrives in a later build."])
    }

    @Test("the attention badge is set by a posted notification, survives a relaunch, and clears only when a Review window opens")
    func badge() async throws {
        let router = FakeRouter()
        let model = try makeModel(runner: nil, data: data, defaults: defaults, router: router)
        model.notificationPosted()
        #expect(model.iconState.mark == .badge)

        let relaunched = try makeModel(runner: nil, data: data, defaults: defaults, router: router)
        #expect(relaunched.badgePending)
        relaunched.review()
        #expect(relaunched.badgePending, "no window opened")
        router.opens = true
        relaunched.review()
        #expect(!relaunched.badgePending)
        #expect(!AppPreferences(defaults: defaults.defaults).badgePending)
    }

    @Test("the class switches change Selected to clean, never the size next to the icon")
    func switchesLeaveTheIcon() async throws {
        try data.write(lastScan: try Fixture.data("scan-report-groups"))
        try data.write(settings: try String(contentsOf: Fixture.url("scan-report-groups.settings"), encoding: .utf8))
        defaults.defaults.set(true, forKey: AppPreferences.showSizeKey)
        let model = try makeModel(runner: nil, data: data, defaults: defaults)
        await model.start()
        let label = try #require(model.iconState.label)
        let selected = model.selectedToClean
        model.includeSafe = false
        model.includeRebuildable = false
        #expect(model.selectedToClean == 0 && selected > 0)
        #expect(model.iconState.label == label)
        #expect(model.iconState.mark == .dot)
    }

    @Test("with the badge preference off the badge is never drawn")
    func badgeOff() async throws {
        defaults.defaults.set(false, forKey: AppPreferences.showBadgeKey)
        let model = try makeModel(runner: nil, data: data, defaults: defaults)
        model.notificationPosted()
        #expect(model.iconState.mark != .badge)
    }

    @Test("the size preference comes from UserDefaults and is off by default")
    func sizePreference() throws {
        #expect(!AppPreferences(defaults: defaults.defaults).showSize)
        defaults.defaults.set(true, forKey: AppPreferences.showSizeKey)
        #expect(AppPreferences(defaults: defaults.defaults).showSize)
    }

    @Test("when shared space makes the selection free less, the popover says how much it frees")
    func freesAbout() async throws {
        try data.write(lastScan: try Fixture.editedFindings("scan-report-groups") { findings in
            findings[0]["private_bytes"] = 1024
        })
        let model = try makeModel(runner: nil, data: data, defaults: defaults)
        await model.start()
        let report = try #require(model.loadedReport)
        let npm = report.findings[0]
        #expect(model.summary?.reclaimable(including: [.safe, .rebuildable]) == model.selectedToClean - npm.bytes + 1024)
        #expect(model.freesAbout == ByteSize.human(model.selectedToClean - npm.bytes + 1024))
        model.includeSafe = false
        #expect(model.freesAbout == nil, "without the npm cache nothing is shared")
    }

    // MARK: Next scan

    @Test("the footer's next scan is the schedule seam's slot")
    func nextScan() throws {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = try #require(TimeZone(identifier: "UTC"))
        calendar.locale = Locale(identifier: "en_US_POSIX")
        let now = try #require(calendar.date(from: DateComponents(year: 2026, month: 10, day: 19, hour: 9)))  // a Monday
        func at(_ day: Int, _ hour: Int) throws -> Date {
            try #require(calendar.date(from: DateComponents(year: 2026, month: 10, day: day, hour: hour)))
        }
        #expect(Wording.nextScan(nil, now: now, calendar: calendar) == "Scheduled scans off")
        #expect(Wording.nextScan(try at(19, 13), now: now, calendar: calendar) == "Next scan today, 13:00")
        #expect(Wording.nextScan(try at(20, 10), now: now, calendar: calendar) == "Next scan tomorrow, 10:00")
        #expect(Wording.nextScan(try at(26, 10), now: now, calendar: calendar) == "Next scan Monday, 10:00")

        let model = try makeModel(runner: nil, data: data, defaults: defaults)
        #expect(model.nextScanText == "Scheduled scans off", "nothing schedules scans before the background story")
    }
}
