import Foundation
import Testing
@testable import DDCKit

/// Story "Review and clean window": the popover's requests, the sheet's dry
/// run and the clean, against `FakeCLI` replaying what the real CLI printed
/// over a synthetic home (`clean-dry-run.jsonl`, `clean-report.json`).
/// The clean was recorded with the delete items only, so the recording never
/// touched a real Trash; `py1/.venv` (deleted) and `.ollama/models` (trashed)
/// were then added to the report by hand, so it answers the whole list the
/// test sends. Temporary directories only: no real Trash, data directory or TCC.db.
@Suite("Review window: the popover's requests")
@MainActor
struct ReviewRequestTests {
    let data: TempDataDirectory
    let defaults: TempDefaults

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
        try data.write(lastScan: Fixture.data("scan-report-groups"))
        try data.write(settings: String(decoding: Fixture.data("scan-report-groups.settings"), as: UTF8.self))
    }

    /// The popover and the window wired as the app wires them.
    private func wire() async throws -> (PopoverModel, ReviewModel, FakeRunner, ReviewEvents) {
        let runner = FakeRunner(scan: FakeRunner.done(Data()), rules: FakeRunner.done(try Fixture.data("rules")))
        let events = ReviewEvents()
        var popover: PopoverModel?
        let review = makeReview(runner: runner, data: data, defaults: defaults, events: events) { popover?.snapshot }
        let model = PopoverModel(
            runner: runner,
            data: data.directory,
            probe: try data.probe(granted: true),
            preferences: AppPreferences(defaults: defaults.defaults),
            router: review
        )
        popover = model
        await model.start()
        return (model, review, runner, events)
    }

    @Test("a biggest find opens the window on its group, with the popover's selection")
    func group() async throws {
        let (popover, review, runner, events) = try await wire()
        popover.review(group: "node_modules")
        #expect(events.presented == 1)
        #expect(review.mode == .groups)
        #expect(review.entry == ReviewItems.Entry(riskClass: .rebuildable, group: "node_modules"))
        #expect(review.sheet == nil)
        #expect(await runner.count(.dryRun) == 0)
        #expect(review.selectedBytes == popover.selectedToClean)
    }

    @Test("Clean selected… opens the sheet on exactly Selected to clean, starts its dry run and never a clean; the badge clears")
    func cleanSelected() async throws {
        let (popover, review, runner, _) = try await wire()
        popover.notificationPosted()
        popover.includeSafe = false
        popover.cleanSelected()

        let sheet = try #require(review.sheet)
        #expect(sheet.stage == .confirm)
        #expect(sheet.confirmation.bytes == popover.selectedToClean)
        #expect(sheet.confirmation.items.count == popover.summary?.row(.rebuildable)?.selectedCount)
        #expect(Set(sheet.confirmation.items.map(\.finding.class)) == [.rebuildable])
        #expect(review.selectedBytes == popover.selectedToClean)
        try await until("the dry run") { review.sheet?.warnings == .checked }
        #expect(await runner.calls.contains(.dryRun))
        #expect(await !runner.calls.contains(.clean))
        #expect(!popover.badgePending)

        // While the sheet shows, a request only brings the window forward.
        popover.includeSafe = true
        popover.cleanSelected()
        #expect(review.sheet?.confirmation == sheet.confirmation)
    }

    @Test("a type switched off in Settings while the window is open leaves the selection at once")
    func settingsChange() async throws {
        let (popover, review, _, _) = try await wire()
        popover.review()
        #expect(review.isSelected(2))
        // The file's date moves on, as when the Settings window writes it.
        try data.write(settings: #"{"schema_version": 1, "disabled_rules": ["cache.pip", "app.chrome-cache"], "min_size": 65536}"#)
        try FileManager.default.setAttributes([.modificationDate: Date().addingTimeInterval(5)], ofItemAtPath: data.directory.settings.path)
        review.refreshSettings()
        #expect(!review.isSelected(2))
        #expect(review.items?.row(2)?.lock == .typeOff)
        review.openSheet()
        #expect(review.sheet?.confirmation.items.contains { $0.id == 2 } == false)
    }
}

@Suite("Review window: cleaning", .timeLimit(.minutes(1)))
@MainActor
struct CleanRunTests {
    let data: TempDataDirectory
    let defaults: TempDefaults
    let fake: FakeCLI
    let runner: CLIRunner
    let events = ReviewEvents()
    let review: ReviewModel
    let items: ReviewItems

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
        try data.write(lastScan: Fixture.data("scan-report-groups"))
        fake = try FakeCLI(lastScan: data.directory.lastScan)
        try fake.put("dry-run.jsonl", Data(contentsOf: Fixture.url("clean-dry-run", extension: "jsonl")))
        try fake.put("clean-report.json", Fixture.data("clean-report"))
        runner = CLIRunner(executable: fake.executable)
        let snapshot = try ReviewFixture.snapshot(settingsIn: data)
        review = makeReview(runner: runner, data: data, defaults: defaults, events: events) { snapshot }
        _ = review.open(ReviewRequest(group: nil, classes: [.safe, .rebuildable], confirm: false))
        items = try #require(review.items)
        review.selection = try ReviewFixture.recordedSelection(items)
    }

    private func id(_ suffix: String) throws -> Int { try ReviewFixture.id(items, suffix) }

    /// The sheet with its dry run done.
    private func checkedSheet() async throws {
        review.openSheet()
        try await until("the dry run") { review.sheet?.warnings == .checked }
    }

    private func runs(_ prefix: String) throws -> Int {
        try fake.runs().filter { $0.event == "start" && $0.args.hasPrefix(prefix) }.count
    }

    @Test("the sheet shows the predicted skips, then preparing, cleaning and the result with skips grouped by cause")
    func flow() async throws {
        try fake.put("rescan.json", Fixture.data("scan-report-groups"))
        try await checkedSheet()
        let chrome = try id("Google/Chrome")
        #expect(review.sheet?.predictions[chrome] == .wouldSkip(.ownerRunning(pid: 4242, name: "Google Chrome")))
        #expect(review.sheet?.predictions[try id(".npm/_cacache")] == .wouldDelete)
        #expect(review.sheet?.predictions[try id(".ollama/models")] == .wouldTrash)

        // The list the CLI was given is the one the sheet showed.
        struct Listed: Decodable { let items: [Item]; struct Item: Decodable { let path: String } }
        let dryList = try JSONDecoder().decode(Listed.self, from: try #require(try fake.lists().first))
        #expect(dryList.items.map(\.path) == review.sheet?.confirmation.items.map(\.finding.path))

        review.cleanConfirmedItems()
        review.cleanConfirmedItems()  // twice: one clean
        try await until("preparing") { review.sheet?.stage.isPreparing == true }
        #expect(events.busy == [true])
        try fake.open("clean-start")
        let replaced = Date()
        try await until("the cleaning phase") { review.sheet?.stage.isCleaningPhase == true }
        #expect(Date().timeIntervalSince(replaced) < 1)
        try fake.open("clean-go")
        try await until("the result") { review.sheet?.stage.result != nil }

        let result = try #require(review.sheet?.stage.result)
        #expect(result.deleted == CleanResult.Tally(count: 4, bytes: 901_120 + 602_112 + 507_904 + 352_256))
        #expect(result.trashed == CleanResult.Tally(count: 1, bytes: 700_416))
        #expect(result.skipped == CleanResult.Tally(count: 1, bytes: 303_104))
        #expect(result.causes == [CleanResult.Cause(title: "Google Chrome was running", count: 1, bytes: 303_104, paths: ["~/Library/Caches/Google/Chrome"])])
        #expect(result.volumes.map(\.name) == ["The startup disk"])
        #expect(result.volumes.first?.freeAfter == 200_001_871_872)
        try await until("the rescan") { events.busy == [true, false] }
        #expect(events.rescans == 1)
        #expect(try runs("clean --items") == 2, "one dry run, one clean")
        let listed = try fake.lists()
        #expect(listed.count == 2 && listed[0] == listed[1], "the clean got the list the sheet showed")

        // Done: what the report lists as deleted can no longer be selected; the skip stays selected.
        review.closeSheet()
        for suffix in [".npm/_cacache", "p1/node_modules", "py1/.venv", "r1/target", ".ollama/models"] {
            #expect(review.items?.row(try id(suffix))?.lock == .cleaned)
            #expect(!review.isSelected(try id(suffix)))
        }
        #expect(review.isSelected(chrome))
    }

    /// stderr lines for the fake to play, one file per gate `events-<k>`.
    private func play(_ k: Int, _ lines: [String]) throws {
        try fake.put("clean-events-\(k).jsonl", Data(lines.map { $0 + "\n" }.joined().utf8))
    }

    @Test("the clean's events drive the sheet: step 1's counters, cleaning with last-scan.json unchanged, then item i of n")
    func events() async throws {
        let npm = ProgressTests.current(1, "/Users/example/.npm/_cacache", "delete", 901_120)
        let p1 = ProgressTests.current(3, "/Users/example/p1/node_modules", "delete", 602_112)
        // No rescan.json: the file never changes, so only the events move the phase.
        try play(1, [ProgressTests.preparing, ProgressTests.walk("recheck", 45_600, expected: 71_300)])
        try play(2, [
            ProgressTests.walk("recheck", 71_250, expected: 71_300),
            ProgressTests.walk("markers", 120, expected: 400),
            ProgressTests.cleaning,
            ProgressTests.items(done: 0, n: 6, bytesDone: 0, bytesTotal: 3_366_912, current: npm),
            ProgressTests.item(1, n: 6, "/Users/example/.npm/_cacache", "delete", 901_120, #""outcome":"deleted""#),
            ProgressTests.item(2, n: 6, "/Users/example/Library/Caches/Google/Chrome", "delete", 303_104,
                               #""outcome":"skipped","reason":"owner-running","pid":4242,"name":"Google Chrome""#),
            ProgressTests.items(done: 2, n: 6, bytesDone: 1_204_224, bytesTotal: 3_366_912, current: p1),
        ])
        // Hand-written, not from the recorded report: a native-only item in flight under its tool.
        let docker = ProgressTests.current(6, "/Users/example/Library/Containers/com.docker.docker", "native-only", 4_294_967_296, tool: "docker")
        try play(3, [ProgressTests.items(done: 5, n: 6, bytesDone: 2_666_496, bytesTotal: 3_366_912, current: docker)])
        let modified = DataDirectory.modified(data.directory.lastScan)
        try await checkedSheet()
        review.cleanConfirmedItems()
        try await until("preparing") { review.sheet?.stage.isPreparing == true }
        #expect(review.sheet?.progress == nil, "no event yet")
        try fake.open("clean-start")
        try fake.open("events-1")
        try await until("step 1's counters") { review.sheet?.progress?.walk?.files == 45_600 }
        #expect(CleanWording.walkLine(try #require(review.sheet?.progress?.walk)) == "45,600 of about 71,300 files read")
        #expect(review.sheet?.stage.isPreparing == true)

        try fake.open("events-2")
        try await until("item 3 in flight") { review.sheet?.progress?.current?.i == 3 }
        #expect(review.sheet?.stage.isCleaningPhase == true)
        #expect(DataDirectory.modified(data.directory.lastScan) == modified, "the event, not the file, switched the phase")
        let progress = try #require(review.sheet?.progress)
        #expect(progress.done == 2 && progress.n == 6 && progress.skipped == 1)
        #expect(progress.current?.path == "/Users/example/p1/node_modules")
        #expect(progress.fraction == 1_204_224.0 / 3_366_912)
        #expect(progress.inFlight == 602_112.0 / 3_366_912)
        #expect(CleanWording.counterLine(progress) == "1.1 of 3.2 MiB processed · 2 of 6 items · 1 skipped")
        let run = try #require(try fake.runs().last { $0.event == "start" })
        #expect(run.args.hasSuffix("--json --progress json"))

        try fake.open("events-3")
        try await until("the tool in flight") { review.sheet?.progress?.current?.i == 6 }
        #expect(review.sheet?.progress?.current?.tool == "docker")
        #expect(review.sheet?.progress?.inFlight == 0, "a native-only item adds no stripe")
        #expect(review.sheet?.progress?.fraction == 2_666_496.0 / 3_366_912)

        try fake.open("clean-go")
        try await until("the result") { review.sheet?.stage.result != nil }
        try await until("the rescan") { events.busy == [true, false] }
        #expect(events.rescans == 1)
    }

    @Test("a crash after the cleaning event points to the journal and rescans, with last-scan.json unchanged")
    func crashAfterCleaningEvent() async throws {
        try play(1, [ProgressTests.preparing, ProgressTests.cleaning])
        try fake.open("events-1")
        try fake.open("clean.crash")
        try fake.open("clean-start")
        try fake.open("clean-go")
        let modified = DataDirectory.modified(data.directory.lastScan)
        try await checkedSheet()
        review.cleanConfirmedItems()
        try await until("the end") { review.sheet?.stage.stopped != nil }
        let stopped = try #require(review.sheet?.stage.stopped)
        #expect(stopped.text.contains("the journal lists what was done"))
        #expect(stopped.journal == data.directory.journal)
        #expect(DataDirectory.modified(data.directory.lastScan) == modified)
        try await until("the rescan") { events.rescans == 1 }
    }

    @Test("while scans are paused Scan again starts nothing, and a clean still asks for its rescan")
    func whilePaused() async throws {
        events.paused = true
        #expect(review.scansPaused)
        await review.scanAgain()
        #expect(events.rescans == 0 && review.rescanning == nil)

        try fake.put("rescan.json", Fixture.data("scan-report-groups"))
        try await checkedSheet()
        review.cleanConfirmedItems()
        try fake.open("clean-start")
        try fake.open("clean-go")
        try await until("the rescan") { events.busy == [true, false] }
        #expect(events.rescans == 1)
    }

    @Test("each warning appears on its item as its dry-run line arrives")
    func delayedLines() async throws {
        try fake.open("dry-run.gated")
        review.openSheet()
        #expect(review.sheet?.stage == .confirm, "the sheet shows before any line")
        try await until("the dry run started") { review.sheet?.warnings == .checking }
        #expect(review.sheet?.predictions.isEmpty == true)

        try fake.open("line-0")
        try await until("line 0") { review.sheet?.predictions[0] != nil }
        #expect(review.sheet?.predictions[try id("Google/Chrome")] == nil)
        try fake.open("line-1")
        try await until("line 1") { review.sheet?.predictions[try! id("Google/Chrome")] != nil }
        #expect(review.sheet?.predictions.count == 2)
        for line in 2...6 { try fake.open("line-\(line)") }
        try await until("the summary") { review.sheet?.warnings == .checked }
        #expect(review.sheet?.predictions.count == 6)
    }

    @Test("a dry run that finds the lock taken says so")
    func dryRunBusy() async throws {
        try fake.put("dry-run.exit", Data("75".utf8))
        review.openSheet()
        try await until("busy") { review.sheet?.warnings == .busy }
    }

    @Test("Cancel on a clean waiting behind a scan: it never starts, the scan goes on, nothing rescans")
    func cancelQueued() async throws {
        try await checkedSheet()
        let scan = Task { await runner.run(.scan, ["wait", "scan-gate"]) }
        try await fake.waitForStart("wait scan-gate")
        review.cleanConfirmedItems()
        #expect(review.sheet?.stage == .cleaning(.waiting))
        #expect(!review.prepareToQuit {}, "a clean that never started does not hold Quit")
        review.quitting = false
        review.cancelClean()
        #expect(review.sheet?.stage.stopped?.text == "Cancelled before it started. Nothing was touched.", "at once, not when the scan ends")
        try fake.open("scan-gate")
        #expect(await scan.value.outcome == .done)
        try await until("idle") { !review.isCleaning }
        #expect(review.sheet?.stage.stopped?.text == "Cancelled before it started. Nothing was touched.")
        #expect(try runs("clean --items") == 1, "only the dry run started")
        #expect(events.rescans == 0)
        #expect(events.busy == [true, false])
    }

    @Test("Cancel while cleaning: SIGTERM, and the report the CLI prints then is the result")
    func cancelRunning() async throws {
        try fake.put("rescan.json", Fixture.data("scan-report-groups"))
        try fake.open("clean.trap")
        try await checkedSheet()
        review.cleanConfirmedItems()
        try await until("preparing") { review.sheet?.stage.isPreparing == true }
        try fake.open("clean-start")
        try await until("the cleaning phase") { review.sheet?.stage.isCleaningPhase == true }
        review.cancelClean()
        try await until("the result") { review.sheet?.stage.result != nil }
        try await until("the rescan") { events.busy == [true, false] }
        #expect(events.rescans == 1)
    }

    @Test("Quit during a clean waits for the CLI to end, not for the rescan after it")
    func quitWaitsForTheProcessOnly() async throws {
        try fake.put("rescan.json", Fixture.data("scan-report-groups"))
        try fake.open("clean.trap")
        events.holdRescan = true
        try await checkedSheet()
        review.cleanConfirmedItems()
        try await until("preparing") { review.sheet?.stage.isPreparing == true }
        try fake.open("clean-start")
        try await until("the cleaning phase") { review.sheet?.stage.isCleaningPhase == true }
        var quit = false
        #expect(review.prepareToQuit { quit = true })
        try await until("the CLI ended") { quit }
        // The callback fires when the process ends, before its report is read.
        try await until("the result") { review.sheet?.stage.result != nil }
        #expect(events.rescans == 0, "no rescan while quitting")
        events.finishRescan()
    }

    @Test("Cancel while preparing ends with nothing touched and no rescan")
    func cancelPreparing() async throws {
        try await checkedSheet()
        review.cleanConfirmedItems()
        try await until("preparing") { review.sheet?.stage.isPreparing == true }
        review.cancelClean()
        try await until("the end") { review.sheet?.stage.stopped != nil }
        #expect(review.sheet?.stage.stopped?.text == "Cancelled. Nothing was touched.")
        #expect(review.sheet?.stage.stopped?.journal == nil)
        #expect(events.rescans == 0)
    }

    @Test("a crash after the CLI saved its rescan points to the journal and rescans; the icon stays busy until that scan ends")
    func crashAfterSave() async throws {
        try fake.put("rescan.json", Fixture.data("scan-report-groups"))
        try fake.open("clean.crash")
        try fake.open("clean-start")
        try fake.open("clean-go")
        events.holdRescan = true
        try await checkedSheet()
        review.cleanConfirmedItems()
        try await until("the end") { review.sheet?.stage.stopped != nil }
        let stopped = try #require(review.sheet?.stage.stopped)
        #expect(stopped.text.contains("the journal lists what was done"))
        #expect(stopped.journal == data.directory.journal)
        try await until("the rescan") { events.rescans == 1 }
        #expect(events.busy == [true], "busy until the rescan ends")
        events.finishRescan()
        try await until("idle") { events.busy == [true, false] }
    }

    @Test("a clean refused before it touched anything (busy, refused) says so and does not rescan")
    func refusedBeforeTouching() async throws {
        for (status, text) in [("75", "Another scan or clean holds the lock"), ("2", "refused by the CLI (exit 2)")] {
            try fake.put("clean.exit", Data(status.utf8))
            try fake.open("clean-start")
            try fake.open("clean-go")
            review.closeSheet()
            try await checkedSheet()
            review.cleanConfirmedItems()
            try await until("the end") { review.sheet?.stage.stopped != nil }
            #expect(review.sheet?.stage.stopped?.text.contains(text) == true)
            #expect(review.sheet?.stage.stopped?.text.hasSuffix("Nothing was touched.") == true)
            try await until("idle") { review.isCleaning == false }
            review.closeSheet()
        }
        #expect(events.rescans == 0)
    }
}
