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
        for version in [2, 4] {
            #expect(throws: ScanReport.ReadError.otherSchema(version)) {
                try FreeReport.read(Fixture.edited("free-report") { $0["schema_version"] = version })
            }
        }
        for reason in ["conda-in-home", "conda-not-writable", "conda-not-scoped"] {
            let refused = try FreeReport.read(Fixture.edited("free-report") { json in
                var actions = json["actions"] as! [[String: Any]]
                actions[0]["action"] = "conda-env-remove"
                actions[0]["area"] = "conda"
                actions[0]["identifier"] = "/opt/miniconda3/envs/ml"
                actions[0]["result"] = ["status": "refused", "reason": reason]
                json["actions"] = actions
            })
            #expect(refused.actions[0].action == .condaEnvRemove)
            #expect(refused.actions[0].result == .refused(try #require(FreeReport.Refusal(rawValue: reason))))
        }

        let list = AreaActionList(actions: [Self.brew.listed, ListedAreaAction(action: .simctlRuntimeDelete, identifier: "A")])
        #expect(String(decoding: try list.encoded(), as: UTF8.self)
            == #"{"actions":[{"action":"brew-cleanup"},{"action":"simctl-runtime-delete","identifier":"A"}],"schema_version":3}"#)
        let conda = AreaActionList(actions: [
            ListedAreaAction(action: .condaEnvRemove, identifier: "/opt/miniconda3/envs/ml"),
            ListedAreaAction(action: .condaClean, identifier: "/opt/miniconda3"),
        ])
        #expect(String(decoding: try conda.encoded(), as: UTF8.self)
            == #"{"actions":[{"action":"conda-env-remove","identifier":"/opt/miniconda3/envs/ml"},{"action":"conda-clean","identifier":"/opt/miniconda3"}],"schema_version":3}"#)
    }

    // MARK: - Which rows offer an action

    /// The offers' row ids, without the conda ones (tested on their own).
    private func notConda(_ offers: [String: AreaActions.Offer]) -> [String] {
        offers.keys.filter { !$0.hasPrefix("conda") }.sorted()
    }

    @Test("Homebrew, a downloaded, idle runtime and the writable conda folders offer an action; nothing else does")
    func offers() throws {
        let offers = AreaActions.offers(try report())
        #expect(offers.keys.sorted() == [
            "conda-env /opt/miniconda3/envs/ml", "conda-env /opt/miniconda3/envs/old", "conda-install /opt/miniconda3",
            "homebrew", "runtime 9A1B2C3D-0000-4000-8000-000000000001",
        ])
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
            #expect(notConda(AreaActions.offers(try runtime(edit))) == ["homebrew"], "\(name)")
        }
        let failed = try report { json in json["simulators"] = ["status": "failed", "message": "simctl failed", "runtimes": [], "listed_bytes": 0] }
        #expect(notConda(AreaActions.offers(failed)) == ["homebrew"])
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

    // MARK: - Conda in /opt

    /// The fixture's `conda` section, edited: its installs as JSON objects.
    private func conda(_ edit: @escaping (inout [[String: Any]]) -> Void) throws -> DiskReport {
        try report { json in
            var section = json["conda"] as! [String: Any]
            var installs = section["installs"] as! [[String: Any]]
            edit(&installs)
            section["installs"] = installs
            json["conda"] = section
        }
    }

    private func rows(_ report: DiskReport) -> [DiskSummary.Row] {
        DiskSummary.make(report, scan: nil, summary: nil, included: [], now: .now).rows
    }

    @Test("each /opt environment is a row nested under its install under Conda, with Remove Environment…; an install's cache offers Clean Package Cache… only when the user can write it")
    func condaRows() throws {
        let all = rows(try report())
        let byID = Dictionary(uniqueKeysWithValues: all.map { ($0.id, $0) })
        let area = try #require(byID["conda"])
        #expect(area.depth == 1 && area.offer == nil, "two installs: the offer is on each install's row")

        let mini = try #require(byID["conda-install /opt/miniconda3"])
        #expect(mini.depth == 2 && mini.title == "/opt/miniconda3")
        #expect(mini.offer?.listed == ListedAreaAction(action: .condaClean, identifier: "/opt/miniconda3"))
        #expect(mini.offer?.buttonTitle == "Clean Package Cache…")
        let ml = try #require(byID["conda-env /opt/miniconda3/envs/ml"])
        #expect(ml.depth == 3 && ml.title == "ml" && ml.size == "2.0 GiB")
        #expect(ml.note.contains("1.0 GiB also held elsewhere, such as the package cache"))
        #expect(ml.offer?.listed == ListedAreaAction(action: .condaEnvRemove, identifier: "/opt/miniconda3/envs/ml"))
        #expect(ml.offer?.buttonTitle == "Remove Environment…" && ml.offer?.bytes == 2 << 30)
        #expect(byID["conda-env /opt/miniconda3/envs/old"]?.note.contains("also held elsewhere") == false)

        // Root-owned: no offer, and the rows say why.
        let ana = try #require(byID["conda-install /opt/anaconda3"])
        #expect(ana.offer == nil && ana.note.contains("your own packages live in ~/.conda/pkgs, a finding in the Review window"))
        let shared = try #require(byID["conda-env /opt/anaconda3/envs/shared"])
        #expect(shared.offer == nil && shared.note.contains("conda needs an administrator"))

        // One install: its environments right under Conda, and the cache's offer on the Conda row.
        let one = rows(try conda { $0 = [$0[0]] })
        let ids = one.map(\.id)
        let at = try #require(ids.firstIndex(of: "conda"))
        #expect(Array(ids[at...].prefix(3)) == ["conda", "conda-env /opt/miniconda3/envs/ml", "conda-env /opt/miniconda3/envs/old"])
        #expect(!ids.contains { $0.hasPrefix("conda-install") })
        #expect(one[at].offer?.action == .condaClean && one[at].offer?.identifier == "/opt/miniconda3")
        #expect(one[at + 1].depth == 2 && one[at + 1].offer?.action == .condaEnvRemove)

        // One install whose cache the user cannot write: no cache offer, and the Conda row says where the user's packages are.
        let locked = rows(try conda { installs in
            var pkgs = installs[0]["packages"] as! [String: Any]
            pkgs["writable"] = false
            installs[0]["packages"] = pkgs
            installs = [installs[0]]
        })
        let lockedArea = try #require(locked.first { $0.id == "conda" })
        #expect(lockedArea.offer == nil)
        #expect(lockedArea.note.contains("Its package cache needs an administrator: your own packages live in ~/.conda/pkgs"))
        #expect(locked.first { $0.id == "conda-env /opt/miniconda3/envs/ml" }?.offer?.action == .condaEnvRemove)

        // No conda section, or a report without the Conda area: no conda rows.
        #expect(!rows(try report { $0["conda"] = nil }).contains { $0.id.hasPrefix("conda-") })
        let noArea = try report { json in
            json["areas"] = (json["areas"] as! [[String: Any]]).filter { $0["id"] as? String != "conda" }
        }
        #expect(AreaActions.offers(noArea).keys.allSatisfy { !$0.hasPrefix("conda") })

        // The environments' sizes are in the area's: the bar does not count them twice.
        let plain = DiskSummary.make(try report { $0["conda"] = nil }, scan: nil, summary: nil, included: [], now: .now)
        let with = DiskSummary.make(try report(), scan: nil, summary: nil, included: [], now: .now)
        #expect(plain.segments == with.segments)
    }

    @Test("the conda sheets name conda, what cannot be undone and what each frees; a removal conda left undone offers Terminal")
    func condaWording() throws {
        let env = AreaActions.Offer(action: .condaEnvRemove, identifier: "/opt/miniconda3/envs/ml", name: "ml", bytes: 1)
        #expect(env.question == "Remove the conda environment ml?")
        #expect(env.confirmTitle == "Remove Environment" && env.tool == "conda")
        #expect(env.consequence.contains("/opt/miniconda3/envs/ml") && env.consequence.contains("cannot be undone"))
        #expect(env.estimateText(1 << 30) == "Only this environment holds 1.0 GiB: that is what removing it frees.")
        let cache = AreaActions.Offer(action: .condaClean, identifier: "/opt/miniconda3", name: "/opt/miniconda3", bytes: 1)
        #expect(cache.question == "Clean the package cache of /opt/miniconda3?")
        #expect(cache.consequence.contains("/opt/miniconda3/pkgs") && cache.consequence.contains("Packages your environments use stay"))
        #expect(cache.estimateText(1 << 30) == "conda says this frees about 1.0 GiB.")
        #expect(cache.runningText == "Running conda clean…" && env.runningText == "Removing the environment…")

        let report = try FreeReport.read(Fixture.data("free-report"))
        func edited(_ action: DiskReport.ActionId, _ result: [String: Any]) throws -> FreeReport.ActionReport {
            try FreeReport.read(Fixture.edited("free-report") { json in
                var actions = json["actions"] as! [[String: Any]]
                actions[0]["action"] = action.rawValue
                actions[0]["area"] = "conda"
                actions[0]["identifier"] = "/opt/miniconda3/envs/ml"
                actions[0]["result"] = result
                json["actions"] = actions
            }).actions[0]
        }
        let kept = try edited(.condaEnvRemove, ["status": "removed-not-freed"])
        #expect(FreeWording.result(kept, name: "ml").hasPrefix("conda finished, but the environment's folder is still there: nothing was freed."))
        #expect(FreeWording.offersTerminal(kept))
        #expect(!FreeWording.offersTerminal(report.actions[0]))
        let failed = try edited(.condaClean, ["status": "failed", "needs_admin": true])
        #expect(FreeWording.result(failed, name: "/opt/miniconda3").hasPrefix("conda clean could not remove everything without an administrator."))
        let refused = try edited(.condaEnvRemove, ["status": "refused", "reason": "conda-not-writable"])
        #expect(FreeWording.result(refused, name: "ml") == "Not run: your user cannot write it: conda needs an administrator.")

        func brew(_ result: [String: Any], output: String? = nil) throws -> FreeReport.ActionReport {
            try FreeReport.read(Fixture.edited("free-report") { json in
                var actions = json["actions"] as! [[String: Any]]
                actions[0]["action"] = "brew-uninstall"
                actions[0]["area"] = "homebrew"
                actions[0]["identifier"] = "wget"
                actions[0]["result"] = result
                actions[0]["output"] = output
                json["actions"] = actions
            }).actions[0]
        }
        let notStarted = try brew(["status": "failed", "needs_admin": false],
                                  output: "brew could not start; nothing was removed\nthe check took 590 s of 600 s; try again")
        #expect(FreeWording.result(notStarted, name: "wget") == "brew could not start, so nothing was removed. the check took 590 s of 600 s; try again.")
        #expect(!FreeWording.offersTerminal(notStarted))
        let uninstallFailed = try brew(["status": "failed", "needs_admin": false], output: "Error: something")
        #expect(FreeWording.result(uninstallFailed, name: "wget") == "brew uninstall failed. You can run the command in Terminal to see why.")
        #expect(FreeWording.offersTerminal(uninstallFailed))
        let kegLeft = try brew(["status": "removed-not-freed"])
        #expect(FreeWording.result(kegLeft, name: "wget").hasPrefix("brew finished, but a version is still in /opt/homebrew/Cellar"))
        #expect(FreeWording.offersTerminal(kegLeft))
        let needed = try brew(["status": "refused", "reason": "brew-needed"])
        #expect(FreeWording.result(needed, name: "wget") == "Not run: something installed still needs it.")
    }

    // MARK: - The runs

    @Test("the sheet shows the tool's dry run, then runs exactly the confirmed action")
    func previewThenRun() async throws {
        let m = model()
        var edited = 0
        m.scanReportEdited = { edited += 1 }
        await m.load()
        await m.previewAction(Self.brew)
        guard case .confirm(let previews)? = m.freeSheet?.stage, let preview = previews.first, previews.count == 1 else {
            Issue.record("no confirmation: \(String(describing: m.freeSheet))")
            return
        }
        #expect(preview.preview?.lines.count == 2)

        let run = Task { await m.freeConfirmedActions() }
        try await until { (try? fake.runs().filter { $0.event == "start" }.count) == 2 }
        #expect(m.freeRunning && !m.canScan && !m.canFree)
        try fake.open("free-go")
        await run.value

        guard case .result(let results)? = m.freeSheet?.stage, let result = results.first, results.count == 1 else {
            Issue.record("no result: \(String(describing: m.freeSheet))")
            return
        }
        #expect(FreeWording.result(result, name: "Homebrew") == "Freed \(ByteSize.human(2_147_483_648 - 858_993_460)).")
        #expect(m.freeResults[Self.brew.id] == result)
        #expect(edited == 1, "the popover reads the edited last-scan.json again")
        #expect(try fake.actionLists().map { String(decoding: $0, as: UTF8.self) } == Array(
            repeating: #"{"actions":[{"action":"brew-cleanup"}],"schema_version":3}"#, count: 2
        ))
        let runs = try fake.runs().filter { $0.event == "start" }.map(\.args)
        #expect(runs.count == 2 && runs[0].hasSuffix("--dry-run --json") && runs[1].hasSuffix(".json --json"))
        let leftover = try FileManager.default.contentsOfDirectory(atPath: fake.directory.path).filter { $0.hasPrefix("ddc-actions-") }
        #expect(leftover.isEmpty, "the list file is removed after each run")

        await m.closeFree()
        #expect(m.freeSheet == nil && m.canScan && m.canFree)
    }

    /// The run report's action, as the CLI writes it for `action` on `identifier`.
    private func reported(_ action: DiskReport.ActionId, _ identifier: String?, _ result: [String: Any] = ["status": "freed"]) throws
        -> FreeReport.ActionReport
    {
        try FreeReport.read(Fixture.edited("free-report") { json in
            var actions = json["actions"] as! [[String: Any]]
            actions[0]["action"] = action.rawValue
            actions[0]["area"] = action == .simctlRuntimeDelete ? "simulator-runtimes" : action == .brewCleanup ? "homebrew" : "conda"
            actions[0]["identifier"] = identifier
            actions[0]["result"] = result
            json["actions"] = actions
        }).actions[0]
    }

    @Test("a report that does not match the list one for one, in count, order, action or identifier, is a failure, never a partial result")
    func mismatchedReport() async throws {
        let edits: [(String, (inout [String: Any]) -> Void)] = [
            ("two for one", { json in
                let actions = json["actions"] as! [[String: Any]]
                json["actions"] = actions + actions
            }),
            ("none", { $0["actions"] = [[String: Any]]() }),
            ("another action", { json in
                var actions = json["actions"] as! [[String: Any]]
                actions[0]["action"] = "simctl-runtime-delete"
                actions[0]["area"] = "simulator-runtimes"
                actions[0]["identifier"] = "9A1B2C3D-0000-4000-8000-000000000001"
                json["actions"] = actions
            }),
            ("an identifier", { json in
                var actions = json["actions"] as! [[String: Any]]
                actions[0]["identifier"] = "other"
                json["actions"] = actions
            }),
            ("not a dry run", { $0["dry_run"] = false }),
        ]
        for (name, edit) in edits {
            try fake.put("free-preview.json", Fixture.edited("free-preview", edit))
            let m = model()
            await m.previewAction(Self.brew)
            #expect(m.freeSheet?.stage == .failed("The CLI's report does not match the confirmed action."), "\(name)")
            #expect(m.freeResults.isEmpty)
        }

        let a = AreaActions.Offer(action: .simctlRuntimeDelete, identifier: "A", name: "iOS 26.0", bytes: 1)
        let b = AreaActions.Offer(action: .simctlRuntimeDelete, identifier: "B", name: "iOS 27.0", bytes: 1)
        let ra = try reported(.simctlRuntimeDelete, "A")
        let rb = try reported(.simctlRuntimeDelete, "B")
        #expect(DiskModel.matches([ra, rb], [a, b]))
        #expect(!DiskModel.matches([rb, ra], [a, b]), "another order")
        #expect(!DiskModel.matches([ra], [a, b]), "one short")
        #expect(!DiskModel.matches([ra, ra], [a, b]), "another identifier")
        #expect(!DiskModel.matches([ra, try reported(.condaClean, "B")], [a, b]), "another action")
    }

    /// Story "Sparkle updates: check daily, Update and Relaunch, and the cask's auto_updates".
    @Test("while an update's install waits, no measure, preview or run starts, and an open sheet's confirm runs nothing")
    func whileAnUpdateWaits() async throws {
        let m = model()
        await m.load()
        await m.previewAction(Self.brew)
        guard case .confirm? = m.freeSheet?.stage else {
            Issue.record("no confirmation: \(String(describing: m.freeSheet))")
            return
        }
        m.setUpdateWaiting(true)
        await m.freeConfirmedActions()
        guard case .confirm? = m.freeSheet?.stage else {
            Issue.record("the confirm ran: \(String(describing: m.freeSheet))")
            return
        }
        #expect(try fake.runs().filter { $0.event == "start" }.count == 1, "the dry run only")
        await m.closeFree()
        #expect(!m.canFree && !m.canScan)
        await m.previewAction(Self.brew)
        #expect(m.freeSheet == nil)
        await m.scanDisk()
        #expect(try fake.runs().filter { $0.event == "start" }.count == 1)

        m.setUpdateWaiting(false)
        #expect(m.canFree && m.canScan)
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
        guard case .result(let results)? = m.freeSheet?.stage, let result = results.first else {
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

    @Test("Escape closes the sheet at once and stops its dry run; not during a run")
    func dismissDuringPreview() async throws {
        try fake.put("free-preview.gated", Data())
        let m = model()
        let preview = Task { await m.previewAction(Self.brew) }
        try await until { (try? fake.runs().contains { $0.event == "start" }) == true }
        m.dismissFree()
        #expect(m.freeSheet == nil, "closed before anything is awaited")
        await preview.value
        #expect(m.freeSheet == nil)
        #expect(!(try fake.runs().contains { $0.event == "start" && !$0.args.contains("--dry-run") }))

        try fake.open("free-preview-go")
        await m.previewAction(Self.brew)
        let run = Task { await m.freeConfirmedActions() }
        try await until { (try? fake.runs().filter { $0.event == "start" }.count) == 3 }
        m.dismissFree()
        #expect(m.freeRunning, "a run keeps its sheet")
        try fake.open("free-go")
        await run.value
        m.dismissFree()
        #expect(m.freeSheet == nil)
    }
}
