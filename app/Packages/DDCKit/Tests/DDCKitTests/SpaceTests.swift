import Foundation
import Testing
@testable import DDCKit

/// A runner whose runs of the held kinds wait until the test releases them;
/// every run answers with the reply of its kind, done and empty by default.
actor HoldingRunner: CLIRunning {
    private(set) var calls: [CLIRunner.Kind] = []
    private var held: Set<CLIRunner.Kind>
    private var waiting: [CheckedContinuation<Void, Never>] = []
    private var replies: [CLIRunner.Kind: Data] = [:]

    init(holding held: Set<CLIRunner.Kind> = []) {
        self.held = held
    }

    func run(_ kind: CLIRunner.Kind, _ arguments: [String], options: CLIRunner.Options) async -> CLIRunner.Result {
        calls.append(kind)
        if held.contains(kind) { await withCheckedContinuation { waiting.append($0) } }
        return FakeRunner.done(replies[kind] ?? Data())
    }

    func cancel(_ ticket: CLIRunner.Ticket) async {}

    func reply(_ kind: CLIRunner.Kind, _ data: Data) {
        replies[kind] = data
    }

    func count(_ kind: CLIRunner.Kind) -> Int {
        calls.filter { $0 == kind }.count
    }

    /// Holds the later runs of `kind` too.
    func hold(_ kind: CLIRunner.Kind) {
        held.insert(kind)
    }

    /// Lets every held run return; later runs are held no more.
    func release() {
        held = []
        for run in waiting { run.resume() }
        waiting = []
    }

    func waitHeld() async throws {
        for _ in 0..<1_000 {
            if !waiting.isEmpty { return }
            try await Task.sleep(for: .milliseconds(5))
        }
        throw CancellationError()
    }
}

/// Story "Popover: say when used space grew since the last scan": the
/// `space --json` report, the band, the growth badge and when the app asks.
/// Fakes only: no real `ddc space`, temporary data directories and defaults.
@Suite("Used space grew", .timeLimit(.minutes(1)))
@MainActor
struct SpaceTests {
    let data: TempDataDirectory
    let defaults: TempDefaults

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
    }

    /// The fixtures' `checked_at`: 26 h after the scan baseline of `space-grew`.
    static let checked = Date(timeIntervalSince1970: 1_793_700_000)  // 2026-11-03T10:00:00Z
    static let scanAt = "2026-11-02T08:00:00Z"
    static let cleanAt = "2026-11-03T07:15:00Z"
    static let mount = "/System/Volumes/Data"

    private func space(_ name: String, _ edit: (inout [String: Any]) -> Void = { _ in }) throws -> SpaceReport {
        try SpaceReport.read(Fixture.edited(name, edit))
    }

    /// The groups report, scanned at `generatedAt`, its volume read again by a clean at `readAt`.
    private func writeReport(generatedAt: String = Self.scanAt, readAt: String? = nil, bump: TimeInterval = 0) throws {
        let report = try Fixture.edited("scan-report-groups") { json in
            json["generated_at"] = generatedAt
            if let readAt, var volumes = json["volumes"] as? [[String: Any]] {
                volumes[0]["read_at"] = readAt
                json["volumes"] = volumes
            }
        }
        try data.write(lastScan: report)
        // Another mtime, so a refresh reads it again.
        try FileManager.default.setAttributes([.modificationDate: Date.now.addingTimeInterval(bump)], ofItemAtPath: data.directory.lastScan.path)
    }

    private func model(_ runner: (any CLIRunning)?, clock: Clock = Clock(Self.checked)) throws -> PopoverModel {
        try makeModel(runner: runner, data: data, defaults: defaults, clock: { clock.now })
    }

    private var preferences: AppPreferences { AppPreferences(defaults: defaults.defaults) }

    // MARK: - The report

    @Test("the CLI's fixtures decode: grew against a scan, low disk against a clean, quiet with no baseline")
    func decodes() throws {
        let grew = try SpaceReport.read(Fixture.data("space-grew"))
        #expect(grew.state == .grew && grew.grew)
        #expect(grew.volume == .init(mountPoint: Self.mount, totalBytes: 536_870_912_000, freeBytes: 107_374_182_400))
        #expect(grew.baseline?.source == .scanned && grew.baseline?.at == Self.scanAt)
        #expect(grew.inUseDelta == 212_600_881_152)

        let low = try SpaceReport.read(Fixture.data("space-low-disk"))
        #expect(low.state == .lowDisk && low.grew)
        #expect(low.baseline?.source == .cleaned && low.baseline?.at == Self.cleanAt)

        let quiet = try SpaceReport.read(Fixture.data("space-quiet"))
        #expect(quiet.state == .quiet && !quiet.grew)
        #expect(quiet.baseline == nil && quiet.inUseDelta == nil)
    }

    @Test("an unknown state, another schema version, or a dropped baseline key is refused; an unknown key is not")
    func strict() throws {
        #expect(throws: (any Error).self) { try self.space("space-grew") { $0["state"] = "shrank" } }
        #expect(throws: ScanReport.ReadError.otherSchema(2)) { try self.space("space-grew") { $0["schema_version"] = 2 } }
        #expect(throws: (any Error).self) { try self.space("space-quiet") { $0.removeValue(forKey: "baseline") } }
        #expect(throws: (any Error).self) { try self.space("space-quiet") { $0.removeValue(forKey: "in_use_delta") } }
        let extra = try space("space-grew") { $0["settled"] = true }
        #expect(extra.state == .grew)
    }

    // MARK: - The band

    @Test("each state's band: grew with Scan Now following canScan, paused gives the number only, low disk with and without growth, null baseline and low disk, quiet none")
    func bandPerState() throws {
        let grew = try #require(GrowthBand.make(try space("space-grew"), now: Self.checked, paused: false, canScan: true))
        #expect(grew.text == "↑ 198.0 GiB more in use on the startup disk than at the last scan, 26 h ago.")
        #expect(grew.detail == "A scan can tell where.")
        #expect(!grew.warning && grew.scanButton == .enabled)
        #expect(GrowthBand.make(try space("space-grew"), now: Self.checked, paused: false, canScan: false)?.scanButton == .disabled)
        let paused = try #require(GrowthBand.make(try space("space-grew"), now: Self.checked, paused: true, canScan: false))
        #expect(paused == GrowthBand(text: grew.text, detail: nil, warning: false, scanButton: .none))

        let afterClean = try space("space-grew") { $0["baseline"] = ["source": "clean", "at": Self.cleanAt, "free_bytes": 1] }
        #expect(GrowthBand.make(afterClean, now: Self.checked, paused: false, canScan: true)?.text
            == "↑ 198.0 GiB more in use on the startup disk than after your clean, 2 h ago.")

        // Low disk and grew: the growth clause, and the scan as for grew.
        let low = try #require(GrowthBand.make(try space("space-low-disk"), now: Self.checked, paused: false, canScan: true))
        #expect(low == GrowthBand(
            text: "The startup disk is 92% full.",
            detail: "↑ 30.0 GiB more in use than after your clean, 2 h ago. A scan can tell where.",
            warning: true, scanButton: .enabled
        ))
        #expect(GrowthBand.make(try space("space-low-disk"), now: Self.checked, paused: false, canScan: false)?.scanButton == .disabled)
        #expect(GrowthBand.make(try space("space-low-disk"), now: Self.checked, paused: true, canScan: false) == GrowthBand(
            text: "The startup disk is 92% full.",
            detail: "↑ 30.0 GiB more in use than after your clean, 2 h ago.",
            warning: true, scanButton: .none
        ), "paused: the number only")
        let lowOnly = try space("space-low-disk") { $0["grew"] = false; $0["in_use_delta"] = 1 << 30 }
        #expect(GrowthBand.make(lowOnly, now: Self.checked, paused: false, canScan: true)
            == GrowthBand(text: "The startup disk is 92% full.", detail: nil, warning: true, scanButton: .none))

        let noBaseline = try space("space-quiet") { json in
            json["state"] = "low-disk"
            json["volume"] = ["mount_point": "/Volumes/Work", "total_bytes": 100 << 30, "free_bytes": 5 << 30]
        }
        #expect(GrowthBand.make(noBaseline, now: Self.checked, paused: false, canScan: true)
            == GrowthBand(text: "Work is 95% full.", detail: nil, warning: true, scanButton: .none))

        #expect(GrowthBand.make(try space("space-quiet"), now: Self.checked, paused: false, canScan: true) == nil)
        let quietWithBaseline = try space("space-grew") { $0["state"] = "quiet"; $0["grew"] = false }
        #expect(GrowthBand.make(quietWithBaseline, now: Self.checked, paused: false, canScan: true) == nil)
    }

    @Test("VoiceOver reads the band without the drawn arrow")
    func spokenBand() throws {
        let grew = try #require(GrowthBand.make(try space("space-grew"), now: Self.checked, paused: false, canScan: true))
        #expect(grew.spokenText == "198.0 GiB more in use on the startup disk than at the last scan, 26 h ago.")
        #expect(grew.spokenDetail == "A scan can tell where.")
        let low = try #require(GrowthBand.make(try space("space-low-disk"), now: Self.checked, paused: false, canScan: true))
        #expect(low.spokenText == "The startup disk is 92% full.")
        #expect(low.spokenDetail == "30.0 GiB more in use than after your clean, 2 h ago. A scan can tell where.")
    }

    @Test("a volume mid-sentence lower-cases only a leading The; the percentage rounds down as the notification's")
    func wording() {
        #expect(Wording.volumeMidSentence("/System/Volumes/Data") == "the startup disk")
        #expect(Wording.volumeMidSentence("/") == "the startup disk")
        #expect(Wording.volumeMidSentence("/Volumes/Work") == "Work")
        #expect(Wording.volumeMidSentence("/Volumes/The Archive") == "the Archive", "only the leading word, as for the startup disk")
        #expect(Wording.percentFull(free: 1, total: 1000) == 99)
        #expect(Wording.percentFull(free: 0, total: 1000) == 100)
        #expect(Wording.percentFull(free: 5, total: 0) == nil)
        let decision = NotifyDecision(notify: true, reason: .lowDisk, volume: Self.mount, reclaimable: 1, free: 42_949_672_960, total: 536_870_912_000)
        #expect(NotificationText.make(decision, groups: []).title == "The startup disk is 92% full")
    }

    @Test("the popover shows the band only for the report it shows: none before the first result, while a scan runs, or against another baseline")
    func bandFollowsTheReport() async throws {
        try writeReport()
        let runner = HoldingRunner()
        await runner.reply(.space, try Fixture.data("space-grew"))
        await runner.reply(.scan, try Fixture.data("scan-report-groups"))
        let model = try model(runner)
        await model.start()
        #expect(model.growthBand == nil, "no result yet")
        await model.refresh()
        let band = try #require(model.growthBand)
        #expect(band.text == "↑ 198.0 GiB more in use on the startup disk than at the last scan, 26 h ago.")
        #expect(band.scanButton == .enabled)
        model.setPaused(true)
        #expect(model.growthBand?.scanButton == GrowthBand.ScanButton.none && model.growthBand?.detail == nil)
        model.setPaused(false)
        model.setUpdateWaiting(true)
        #expect(model.growthBand?.scanButton == .disabled)
        model.setUpdateWaiting(false)

        // Another report on disk than the result's baseline: no band, even before the next result.
        try writeReport(generatedAt: "2026-11-03T09:00:00Z", bump: 60)
        await runner.reply(.space, try Fixture.data("space-quiet"))
        _ = await model.reloadReport()
        #expect(model.space == nil, "dropped at once")
        #expect(model.growthBand == nil)
    }

    @Test("no band while a scan runs")
    func noBandWhileScanning() async throws {
        try writeReport()
        let runner = HoldingRunner(holding: [.scan])
        await runner.reply(.space, try Fixture.data("space-grew"))
        let model = try model(runner)
        await model.start()
        await model.refresh()
        #expect(model.growthBand != nil)
        let scan = Task { await model.scanNow() }
        try await runner.waitHeld()
        #expect(model.growthBand == nil)
        await runner.release()
        await scan.value
    }

    @Test("a scan the app ran drops the old band and its badge as soon as its report is read, before the space check after it")
    func scanDropsTheOldBand() async throws {
        try writeReport()
        let runner = HoldingRunner()
        await runner.reply(.space, try Fixture.data("space-grew"))
        let newer = try Fixture.edited("scan-report-groups") { $0["generated_at"] = "2026-11-03T09:00:00Z" }
        await runner.reply(.scan, newer)
        let model = try model(runner)
        await model.start()
        await model.refresh()
        #expect(model.growthBand != nil && model.growthPending)

        // The CLI saves the report it prints.
        try writeReport(generatedAt: "2026-11-03T09:00:00Z", bump: 60)
        await runner.hold(.space)
        let scan = Task { await model.scanNow() }
        try await runner.waitHeld()
        #expect(await runner.calls.last == .space, "the check after the scan waits")
        #expect(model.space == nil)
        #expect(model.growthBand == nil)
        #expect(!model.growthPending && !preferences.growthBadgePending)
        await runner.release()
        await scan.value
    }

    @Test("a scheduled scan drops the old band and its badge as soon as its report is read")
    func scheduledScanDropsTheOldBand() async throws {
        try writeReport()
        let runner = HoldingRunner()
        await runner.reply(.space, try Fixture.data("space-grew"))
        await runner.reply(.backgroundScan, Data(#"{"schema_version": 1, "status": "scanned", "notify": false, "reason": null, "volume": "/System/Volumes/Data", "reclaimable": 0, "free": 1, "total": 2}"#.utf8))
        let model = try model(runner)
        await model.start()
        await model.refresh()
        #expect(model.growthBand != nil && model.growthPending)

        // The CLI saved the scheduled scan's report; the tick checks the space after it.
        try writeReport(generatedAt: "2026-11-03T09:00:00Z", bump: 60)
        await runner.hold(.space)
        guard case .scanned = await model.scheduledScan() else {
            Issue.record("the scheduled scan did not scan")
            return
        }
        #expect(model.space == nil)
        #expect(model.growthBand == nil)
        #expect(!model.growthPending && !preferences.growthBadgePending)
        let check = Task { await model.checkSpace(force: true) }
        try await runner.waitHeld()
        #expect(model.growthBand == nil && !model.growthPending)
        await runner.release()
        await check.value
    }

    @Test("while the popover is open a result its band shows is recorded, not badged; once closed, the same baseline stays quiet and a new one badges")
    func openPopoverRecords() async throws {
        try writeReport()
        let runner = HoldingRunner()
        await runner.reply(.space, try Fixture.data("space-grew"))
        let model = try model(runner)
        await model.start()
        await model.popoverOpened()
        #expect(model.growthBand != nil)
        #expect(!model.growthPending && !preferences.growthBadgePending)
        #expect(preferences.growthBadgedFor == GrowthBadge.Key(mountPoint: Self.mount, source: .scanned, at: Self.scanAt))

        // Open, and a result for a baseline the popover does not show: the badge says it.
        await runner.reply(.space, try Fixture.edited("space-grew") { $0["baseline"] = ["source": "scan", "at": "2026-11-03T08:30:00Z", "free_bytes": 1] })
        await model.spaceAfterWake()
        #expect(model.growthBand == nil)
        #expect(model.growthPending, "no band to show it")
        await model.popoverOpened()
        #expect(!model.growthPending)

        model.popoverClosed()
        await runner.reply(.space, try Fixture.data("space-grew"))
        await model.spaceAfterWake()
        #expect(model.growthBand != nil)
        #expect(model.growthPending, "the 08:30 baseline was recorded, so the 08:00 one is new again")
        await model.popoverOpened()
        model.popoverClosed()
        await model.spaceAfterWake()
        #expect(!model.growthPending, "closed, same baseline: no badge")

        try writeReport(generatedAt: "2026-11-03T09:00:00Z", bump: 60)
        await runner.reply(.space, try Fixture.edited("space-grew") { $0["baseline"] = ["source": "scan", "at": "2026-11-03T09:00:00Z", "free_bytes": 1] })
        await model.refresh()
        await model.spaceAfterWake()
        #expect(model.growthBand != nil)
        #expect(model.growthPending, "closed, new baseline: the badge")
        #expect(preferences.growthBadgedFor?.at == "2026-11-03T09:00:00Z")
    }

    @Test("with no saved report, low disk shows its band without the growth clause and badges once until a report appears")
    func lowDiskWithoutReport() async throws {
        let runner = HoldingRunner()
        let lowNoBaseline = try Fixture.edited("space-quiet") { json in
            json["state"] = "low-disk"
            json["volume"] = ["mount_point": Self.mount, "total_bytes": 500 << 30, "free_bytes": 10 << 30]
        }
        await runner.reply(.space, lowNoBaseline)
        let model = try model(runner)
        await model.start()
        #expect(model.report == .none)
        await model.refresh()
        #expect(model.growthBand == GrowthBand(text: "The startup disk is 98% full.", detail: nil, warning: true, scanButton: .none))
        #expect(model.growthPending)
        #expect(preferences.growthBadgedFor == GrowthBadge.Key(mountPoint: Self.mount, source: nil, at: nil))
        await model.popoverOpened()
        #expect(!model.growthPending, "the same no-report state does not badge again")
        model.popoverClosed()

        // A report appears: its baseline is new, and a low disk against it badges again.
        try writeReport(bump: 60)
        await runner.reply(.space, try Fixture.edited("space-low-disk") { $0["baseline"] = ["source": "scan", "at": Self.scanAt, "free_bytes": 1] })
        await model.refresh()
        #expect(model.loadedReport != nil)
        #expect(model.growthBand == nil, "the no-report band goes with the report's arrival")
        #expect(!model.growthPending, "the refresh's own check waits ten minutes")
        await model.spaceAfterWake()
        #expect(model.growthPending)
        #expect(preferences.growthBadgedFor == GrowthBadge.Key(mountPoint: Self.mount, source: .scanned, at: Self.scanAt))
    }

    // MARK: - The growth badge

    @Test("the first grew for a baseline sets the badge and records it; the same baseline does not again, a new one does; a relaunch reads the flag")
    func badgeOncePerBaseline() async throws {
        try writeReport()
        let runner = HoldingRunner()
        await runner.reply(.space, try Fixture.data("space-grew"))
        let model = try model(runner)
        await model.start()
        // The CLI keeps no once-per-baseline state: a `ddc space` from a
        // terminal before this changes nothing, and the app's first call badges.
        await model.refresh()
        #expect(model.growthPending)
        #expect(preferences.growthBadgePending)
        #expect(preferences.growthBadgedFor == GrowthBadge.Key(mountPoint: Self.mount, source: .scanned, at: Self.scanAt))
        #expect(model.iconState.mark == .badge)
        #expect(model.iconState.accessibility.contains("used space grew"))

        let relaunched = try self.model(runner)
        #expect(relaunched.growthPending, "a --scheduled instance's flag outlives it")
        #expect(relaunched.iconState.mark == .badge)

        // Opening the popover clears it, and the same baseline does not set it again.
        await model.popoverOpened()
        #expect(!model.growthPending && !preferences.growthBadgePending)
        #expect(model.growthBand != nil, "the band stays while the result holds")
        model.popoverClosed()
        await model.spaceAfterWake()
        #expect(!model.growthPending)
        #expect(model.iconState.mark == .none)

        // A new scan gives a new baseline; growth against it badges again.
        try writeReport(generatedAt: "2026-11-03T09:00:00Z", bump: 60)
        await runner.reply(.space, try Fixture.edited("space-grew") { $0["baseline"] = ["source": "scan", "at": "2026-11-03T09:00:00Z", "free_bytes": 1] })
        await model.popoverOpened()
        #expect(!model.growthPending, "the popover is open")
        model.popoverClosed()
        await model.spaceAfterWake()
        #expect(!model.growthPending, "still the same baseline")
        try writeReport(generatedAt: "2026-11-03T09:30:00Z", bump: 120)
        await runner.reply(.space, try Fixture.edited("space-grew") { $0["baseline"] = ["source": "scan", "at": "2026-11-03T09:30:00Z", "free_bytes": 1] })
        await model.refresh()  // within ten minutes of the last check: the refresh alone does not ask
        #expect(!model.growthPending)
        await model.spaceAfterWake()
        #expect(model.growthPending)
        #expect(preferences.growthBadgedFor?.at == "2026-11-03T09:30:00Z")
    }

    @Test("with Show a badge off the flag is set but the icon shows nothing; the notification's badge keeps its own rule")
    func badgePreference() async throws {
        try writeReport()
        defaults.defaults.set(false, forKey: AppPreferences.showBadgeKey)
        let runner = HoldingRunner()
        await runner.reply(.space, try Fixture.data("space-grew"))
        let model = try model(runner)
        await model.start()
        await model.refresh()
        #expect(model.growthPending && preferences.growthBadgePending)
        #expect(model.iconState.mark == .none)

        model.setShowBadge(true)
        model.notificationPosted()
        #expect(model.iconState.accessibility.contains("new results to review, used space grew"))
        await model.popoverOpened()
        #expect(!model.growthPending)
        #expect(model.badgePending, "only the Review window clears the notification's badge")
        #expect(model.iconState.mark == .badge)
    }

    @Test("a low-disk result badges too, and a reloaded report with another baseline clears the badge and drops the band at once")
    func newBaselineClears() async throws {
        try writeReport(readAt: Self.cleanAt)
        let runner = HoldingRunner()
        await runner.reply(.space, try Fixture.data("space-low-disk"))
        let model = try model(runner)
        await model.start()
        await model.refresh()
        #expect(model.growthPending)
        #expect(model.growthBand?.warning == true)
        let asked = await runner.count(.space)

        // A clean --items edit read the free space again: the old band and badge go before any new result.
        try writeReport(readAt: "2026-11-03T09:45:00Z", bump: 60)
        #expect(await model.reloadReport())
        #expect(model.growthBand == nil && model.space == nil)
        #expect(!model.growthPending && !preferences.growthBadgePending)
        #expect(await runner.count(.space) == asked)

        // A `disk free` edit leaves the volume's baseline: nothing clears.
        await runner.reply(.space, try Fixture.edited("space-low-disk") { $0["baseline"] = ["source": "clean", "at": "2026-11-03T09:45:00Z", "free_bytes": 1] })
        await model.spaceAfterWake()
        #expect(model.growthPending)
        try writeReport(readAt: "2026-11-03T09:45:00Z", bump: 120)
        _ = await model.reloadReport()
        #expect(model.growthPending && model.growthBand != nil)

        // A space result with another baseline clears it too.
        await runner.reply(.space, try Fixture.edited("space-quiet") { $0["baseline"] = ["source": "scan", "at": "2026-11-03T09:59:00Z", "free_bytes": 1]; $0["in_use_delta"] = 0 })
        await model.spaceAfterWake()
        #expect(!model.growthPending)
        #expect(model.growthBand == nil)
    }

    // MARK: - When the app asks

    @Test("no space call while a scan, a scheduled scan or a clean runs, or an update waits; one after the scan or clean the app ran")
    func gating() async throws {
        try writeReport()
        let runner = HoldingRunner(holding: [.scan])
        await runner.reply(.scan, try Fixture.data("scan-report-groups"))
        let model = try model(runner)
        await model.start()

        let scan = Task { await model.scanNow() }
        try await runner.waitHeld()
        await model.checkSpace(force: true)
        await model.popoverOpened()
        await model.spaceAfterWake()
        #expect(await runner.count(.space) == 0)
        await runner.release()
        await scan.value
        #expect(await runner.count(.space) == 1, "after the scan it ran")
        #expect(await runner.calls.last == .space)

        let scheduledRunner = HoldingRunner(holding: [.backgroundScan])
        await scheduledRunner.reply(.backgroundScan, Data(#"{"schema_version": 1, "status": "skipped", "reason": "busy"}"#.utf8))
        let scheduled = try self.model(scheduledRunner)
        let tick = Task { await scheduled.scheduledScan() }
        try await scheduledRunner.waitHeld()
        await scheduled.checkSpace(force: true)
        #expect(await scheduledRunner.count(.space) == 0)
        await scheduledRunner.release()
        _ = await tick.value

        model.setCleaning(true)
        await model.checkSpace(force: true)
        #expect(await runner.count(.space) == 1)
        model.setCleaning(false)
        try await eventually { await runner.count(.space) == 2 }

        model.setUpdateWaiting(true)
        await model.checkSpace(force: true)
        #expect(await runner.count(.space) == 2)
        model.setUpdateWaiting(false)
        await model.checkSpace(force: true)
        #expect(await runner.count(.space) == 3)

        let missing = try self.model(nil)
        await missing.checkSpace(force: true)
        #expect(missing.space == nil)
    }

    @Test("from the refresh at most once per ten minutes by the clock; popover open and wake are not throttled")
    func throttle() async throws {
        let runner = HoldingRunner()
        await runner.reply(.space, try Fixture.data("space-quiet"))
        let clock = Clock(Self.checked)
        let model = try model(runner, clock: clock)
        await model.refresh()
        #expect(await runner.count(.space) == 1)
        clock.now = Self.checked.addingTimeInterval(5 * 60)
        await model.refresh()
        #expect(await runner.count(.space) == 1)
        clock.now = Self.checked.addingTimeInterval(11 * 60)
        await model.refresh()
        #expect(await runner.count(.space) == 2)

        await model.popoverOpened()
        await model.spaceAfterWake()
        #expect(await runner.count(.space) == 4)
        clock.now = Self.checked.addingTimeInterval(15 * 60)
        await model.refresh()
        #expect(await runner.count(.space) == 4, "the forced checks count as the last one")
    }

    @Test("a failed or unreadable space run changes nothing")
    func failures() async throws {
        try writeReport()
        let runner = HoldingRunner()
        await runner.reply(.space, try Fixture.data("space-grew"))
        let model = try model(runner)
        await model.start()
        await model.refresh()
        let band = model.growthBand
        #expect(band != nil)
        await runner.reply(.space, Data("not json".utf8))
        await model.popoverOpened()
        #expect(model.growthBand == band)
        await runner.reply(.space, try Fixture.edited("space-grew") { $0["schema_version"] = 2 })
        await model.spaceAfterWake()
        #expect(model.growthBand == band)
        #expect(!model.growthPending)
    }

    /// Waits for a condition an actor answers.
    private func eventually(_ condition: () async -> Bool) async throws {
        for _ in 0..<1_000 {
            if await condition() { return }
            try await Task.sleep(for: .milliseconds(5))
        }
        Issue.record("timed out")
        throw CancellationError()
    }
}
