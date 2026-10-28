import Foundation
import Testing
@testable import DDCKit

/// Freeing an area with its owning tool from the Whole disk view (story
/// "Whole disk: free space with the owning tool"), over the real runner and
/// a fake CLI. Nothing here runs simctl or brew.
@MainActor
@Suite("Whole disk: free space with the owning tool")
struct DiskFreeTests {
    private let data: TempDataDirectory
    private let fake: FakeCLI
    private let runner: CLIRunner

    init() throws {
        data = try TempDataDirectory()
        fake = try FakeCLI(lastDisk: data.directory.lastDisk)
        runner = CLIRunner(executable: fake.executable)
        try fake.put("disk-report.json", Fixture.data("disk-report"))
        try fake.put("free-preview.json", Fixture.data("free-preview"))
        try fake.put("free-report.json", Fixture.data("free-report"))
        try Fixture.data("disk-report").write(to: data.directory.lastDisk)
    }

    private func model() -> DiskModel {
        DiskModel(runner: runner, data: data.directory, temporaryDirectory: fake.directory)
    }

    private func until(_ condition: () -> Bool) async throws {
        for _ in 0..<1_000 where !condition() {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(condition())
    }

    private static let brew = AreaActions.Offer(action: .brewCleanup, identifier: nil, name: "Homebrew", bytes: 1)

    private func report(_ edit: (inout [String: Any]) -> Void = { _ in }) throws -> DiskReport {
        try DiskReport.read(Fixture.edited("disk-report", edit))
    }

    private func runtime(_ edit: @escaping (inout [String: Any]) -> Void) throws -> DiskReport {
        try report { json in
            var sims = json["simulators"] as! [String: Any]
            var runtimes = sims["runtimes"] as! [[String: Any]]
            edit(&runtimes[0])
            sims["runtimes"] = runtimes
            json["simulators"] = sims
        }
    }

    // MARK: - The report and the list

    @Test("the report decodes strictly; the list has exactly the CLI's keys")
    func contracts() throws {
        let preview = try FreeReport.read(Fixture.data("free-preview"))
        #expect(preview.isDryRun && preview.journal == nil)
        #expect(preview.actions[0].result == .wouldRun)
        #expect(preview.actions[0].preview?.estimateBytes == 1_288_490_188)
        let run = try FreeReport.read(Fixture.data("free-report"))
        #expect(run.actions[0].freedBytes == 2_147_483_648 - 858_993_460)
        #expect(run.lastScan == .edited)

        let failed = try FreeReport.read(Fixture.edited("free-report") { json in
            var actions = json["actions"] as! [[String: Any]]
            actions[0]["result"] = ["status": "failed", "needs_admin": true]
            json["actions"] = actions
        })
        #expect(failed.actions[0].result == .failed(needsAdmin: true))
        #expect(throws: (any Error).self) {
            try FreeReport.read(Fixture.edited("free-report") { json in
                var actions = json["actions"] as! [[String: Any]]
                actions[0]["result"] = ["status": "partly"]
                json["actions"] = actions
            })
        }
        #expect(throws: ScanReport.ReadError.otherSchema(2)) {
            try FreeReport.read(Fixture.edited("free-report") { $0["schema_version"] = 2 })
        }

        let list = AreaActionList(actions: [Self.brew.listed, ListedAreaAction(action: .simctlRuntimeDelete, identifier: "A")])
        #expect(String(decoding: try list.encoded(), as: UTF8.self)
            == #"{"actions":[{"action":"brew-cleanup"},{"action":"simctl-runtime-delete","identifier":"A"}],"schema_version":1}"#)
    }

    // MARK: - Which rows offer an action

    @Test("Homebrew and a downloaded, idle runtime offer an action; nothing else does")
    func offers() throws {
        let offers = AreaActions.offers(try report())
        #expect(offers.keys.sorted() == ["homebrew", "runtime 9A1B2C3D-0000-4000-8000-000000000001"])
        let offered = try #require(offers["runtime 9A1B2C3D-0000-4000-8000-000000000001"])
        #expect(offered.name == "iOS 27.0 (24A100)")
        #expect(offered.listed == ListedAreaAction(action: .simctlRuntimeDelete, identifier: "9A1B2C3D-0000-4000-8000-000000000001"))

        let refusals: [(String, (inout [String: Any]) -> Void)] = [
            ("booted", { $0["booted"] = true }),
            ("not deletable", { $0["deletable"] = false }),
            ("deleting", { $0["state"] = "Deleting" }),
            ("no runtime identifier", { $0.removeValue(forKey: "runtime_identifier") }),
            ("outside AssetsV2", { $0["parent_mount_path"] = "/Library/Developer/CoreSimulator/Volumes/iOS_24A100" }),
            ("climbing out", { $0["parent_mount_path"] = "/System/Library/AssetsV2/../../Library/Developer" }),
        ]
        for (name, edit) in refusals {
            #expect(AreaActions.offers(try runtime(edit)).keys.sorted() == ["homebrew"], "\(name)")
        }
        let failed = try report { json in json["simulators"] = ["status": "failed", "message": "simctl failed", "runtimes": [], "listed_bytes": 0] }
        #expect(AreaActions.offers(failed).keys.sorted() == ["homebrew"])
    }

    @Test("each runtime has a row under its area, and an area an action ran on reads before cleanup")
    func rows() throws {
        let stale = try report { json in
            var areas = json["areas"] as! [[String: Any]]
            for i in areas.indices where areas[i]["id"] as? String == "homebrew" { areas[i]["stale"] = true }
            json["areas"] = areas
        }
        let summary = DiskSummary.make(stale, scan: nil, summary: nil, included: [], now: .now)
        let ids = summary.rows.map(\.id)
        let area = try #require(ids.firstIndex(of: "simulator-runtimes"))
        #expect(ids[area + 1] == "runtime 9A1B2C3D-0000-4000-8000-000000000001")
        let runtimeRow = summary.rows[area + 1]
        #expect(runtimeRow.nested && runtimeRow.title == "iOS 27.0 (24A100)" && runtimeRow.offer?.action == .simctlRuntimeDelete)
        #expect(runtimeRow.note == "1 simulator")
        let brew = try #require(summary.rows.first { $0.id == "homebrew" })
        #expect(brew.size.hasSuffix(" before cleanup"))
        #expect(brew.offer?.action == .brewCleanup)
        #expect(summary.rows.first { $0.id == "applications" }?.offer == nil)

        // The runtime's size is inside its area's: the bar does not count it twice.
        let plain = DiskSummary.make(try report(), scan: nil, summary: nil, included: [], now: .now)
        #expect(plain.segments == summary.segments)

        let booted = DiskSummary.make(try runtime { $0["booted"] = true }, scan: nil, summary: nil, included: [], now: .now)
        let row = try #require(booted.rows.first { $0.id.hasPrefix("runtime ") })
        #expect(row.offer == nil && row.note == "1 simulator · A simulator on it is running")
    }

    // MARK: - The runs

    @Test("the sheet shows the tool's dry run, then runs exactly the confirmed action")
    func previewThenRun() async throws {
        let m = model()
        var edited = 0
        m.scanReportEdited = { edited += 1 }
        await m.load()
        await m.previewAction(Self.brew)
        guard case .confirm(let preview)? = m.freeSheet?.stage else {
            Issue.record("no confirmation: \(String(describing: m.freeSheet))")
            return
        }
        #expect(preview.preview?.lines.count == 2)

        let run = Task { await m.freeConfirmedActions() }
        try await until { (try? fake.runs().filter { $0.event == "start" }.count) == 2 }
        #expect(m.freeRunning && !m.canScan && !m.canFree)
        try fake.open("free-go")
        await run.value

        guard case .result(let result)? = m.freeSheet?.stage else {
            Issue.record("no result: \(String(describing: m.freeSheet))")
            return
        }
        #expect(FreeWording.result(result, name: "Homebrew") == "Freed \(ByteSize.human(2_147_483_648 - 858_993_460)).")
        #expect(m.freeResults[Self.brew.id] == result)
        #expect(edited == 1, "the popover reads the edited last-scan.json again")
        #expect(try fake.actionLists().map { String(decoding: $0, as: UTF8.self) } == Array(
            repeating: #"{"actions":[{"action":"brew-cleanup"}],"schema_version":1}"#, count: 2
        ))
        let runs = try fake.runs().filter { $0.event == "start" }.map(\.args)
        #expect(runs.count == 2 && runs[0].hasSuffix("--dry-run --json") && runs[1].hasSuffix(".json --json"))
        let leftover = try FileManager.default.contentsOfDirectory(atPath: fake.directory.path).filter { $0.hasPrefix("ddc-actions-") }
        #expect(leftover.isEmpty, "the list file is removed after each run")

        await m.closeFree()
        #expect(m.freeSheet == nil && m.canScan && m.canFree)
    }

    @Test("a scan asked for during disk free waits for it and does not fail busy")
    func scanWaits() async throws {
        let m = model()
        await m.previewAction(Self.brew)
        let free = Task { await m.freeConfirmedActions() }
        try await until { (try? fake.runs().filter { $0.event == "start" }.count) == 2 }
        let scan = Task { await runner.run(.scan, ["scan", "--json"]) }
        try await Task.sleep(for: .milliseconds(100))
        #expect(!(try fake.runs().contains { $0.args == "scan --json" }), "the scan has not started")
        try fake.open("free-go")
        await free.value
        let scanned = await scan.value
        #expect(scanned.outcome == .done)
        let events = try fake.runs().map { "\($0.event) \($0.args.split(separator: " ").prefix(2).joined(separator: " "))" }
        #expect(events.suffix(3) == ["end disk free", "start scan --json", "end scan --json"])
    }

    @Test("two disk free runs are two runs: never joined")
    func neverCoalesced() async throws {
        try fake.open("free-go")
        let file = fake.directory.appending(path: "list.json")
        try AreaActionList(actions: [Self.brew.listed]).encoded().write(to: file)
        let args = ["disk", "free", "--actions", file.path, "--json"]
        async let a = runner.run(.diskFree, args)
        async let b = runner.run(.diskFree, args)
        let (first, second) = await (a, b)
        #expect(first.outcome == .done && second.outcome == .done)
        #expect(try fake.runs().filter { $0.event == "start" }.count == 2)
    }

    @Test("no action while the whole disk is measured, and no scan while an action runs")
    func buttonRules() async throws {
        let m = model()
        #expect(m.canFree && m.canScan)
        let scan = Task { await m.scanDisk() }
        try await until { m.scanning }
        #expect(!m.canFree)
        await m.previewAction(Self.brew)
        #expect(m.freeSheet == nil, "no sheet opens while the disk is measured")
        try fake.open("disk-go")
        await scan.value
        #expect(m.canFree)

        let noRunner = DiskModel(runner: nil, data: data.directory)
        #expect(!noRunner.canFree && !noRunner.canScan)
    }

    @Test("a failed run says why; a permission failure offers the Terminal command")
    func failures() async throws {
        let m = model()
        await m.previewAction(Self.brew)
        try fake.put("free.exit", Data("75".utf8))
        try fake.open("free-go")
        await m.freeConfirmedActions()
        #expect(m.freeSheet?.stage == .failed("Another scan or clean holds the lock, perhaps from Terminal. Try again when it ends."))
        await m.closeFree()

        let denied = try FreeReport.read(Fixture.edited("free-report") { json in
            var actions = json["actions"] as! [[String: Any]]
            actions[0]["result"] = ["status": "failed", "needs_admin": true]
            json["actions"] = actions
        }).actions[0]
        #expect(FreeWording.result(denied, name: "Homebrew")
            == "brew cleanup could not remove everything without an administrator. You can run the command in Terminal.")
        #expect(denied.terminalCommand.hasPrefix("HOMEBREW_NO_AUTOREMOVE=1 "))

        let refused = try FreeReport.read(Fixture.edited("free-preview") { json in
            var actions = json["actions"] as! [[String: Any]]
            actions[0]["result"] = ["status": "refused", "reason": "booted"]
            json["actions"] = actions
        }).actions[0]
        #expect(FreeWording.result(refused, name: "iOS 27.0") == "Not run: a simulator on it is running or not yet shut down.")
        for reason in FreeReport.Refusal.allCases { #expect(!reason.text.isEmpty) }
    }

    @Test("Stop during the run: the CLI's report of the stopped tool is the result")
    func stopDuringRun() async throws {
        let stoppedReport = try Fixture.edited("free-report") { json in
            var actions = json["actions"] as! [[String: Any]]
            actions[0]["result"] = ["status": "failed", "needs_admin": false]
            actions[0]["output"] = "stopped before the tool finished\nRemoving: /opt/homebrew/Cellar/wget/1.0"
            json["actions"] = actions
        }
        try fake.put("free-stopped.json", stoppedReport)
        try fake.put("free.trap", Data())
        let m = model()
        await m.previewAction(Self.brew)
        let run = Task { await m.freeConfirmedActions() }
        try await until { (try? fake.runs().filter { $0.event == "start" }.count) == 2 }
        await m.stopFree()
        await run.value
        guard case .result(let result)? = m.freeSheet?.stage else {
            Issue.record("no result: \(String(describing: m.freeSheet))")
            return
        }
        #expect(FreeWording.result(result, name: "Homebrew")
            == "Stopped before brew cleanup finished. Part of it may have been removed; scan the whole disk again to see the space.")
        #expect(m.freeResults[Self.brew.id] == result)
    }

    @Test("the sheet names what brew cleanup removes and what reaches its run")
    func brewConsequence() {
        let text = Self.brew.consequence
        for part in ["/opt/homebrew/Cellar", "~/Library/Caches/Homebrew", "shell profile", "brew.env"] {
            #expect(text.contains(part), "\(part)")
        }
    }

    @Test("a new whole-disk measure clears the action results; no confirm during a scan")
    func measureClearsResults() async throws {
        try fake.open("free-go")
        let m = model()
        await m.previewAction(Self.brew)
        #expect(!m.canScan, "no scan while the sheet waits for its confirmation")
        await m.freeConfirmedActions()
        #expect(m.freeResults[Self.brew.id] != nil)
        await m.closeFree()
        try fake.open("disk-go")
        await m.scanDisk()
        #expect(m.freeResults.isEmpty)
    }

    @Test("closing the sheet during the dry run stops it; nothing runs")
    func closeDuringPreview() async throws {
        try fake.put("free-preview.gated", Data())
        let m = model()
        let preview = Task { await m.previewAction(Self.brew) }
        try await until { (try? fake.runs().contains { $0.event == "start" }) == true }
        await m.closeFree()
        await preview.value
        #expect(m.freeSheet == nil)
        #expect(!(try fake.runs().contains { $0.event == "start" && !$0.args.contains("--dry-run") }))
    }
}
