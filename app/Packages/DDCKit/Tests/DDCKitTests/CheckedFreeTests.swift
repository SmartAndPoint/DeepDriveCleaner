import Foundation
import Testing
@testable import DDCKit

/// Checking rows of the Whole disk view and freeing them in one sheet
/// (story "Whole disk: free the checked areas in one sheet"), over the real
/// runner and a fake CLI. Nothing here runs simctl, brew or conda.
@MainActor
@Suite("Whole disk: free the checked areas in one sheet")
struct CheckedFreeTests {
    private let data: TempDataDirectory
    private let fake: FakeCLI
    private let runner: CLIRunner

    /// The fixture's runtime, and a second one this suite adds after it.
    static let first = "9A1B2C3D-0000-4000-8000-000000000001"
    static let second = "9A1B2C3D-0000-4000-8000-000000000002"

    init() throws {
        data = try TempDataDirectory()
        fake = try FakeCLI(lastDisk: data.directory.lastDisk)
        runner = CLIRunner(executable: fake.executable)
        try fake.put("disk-report.json", Self.twoRuntimes())
        try write(Self.twoRuntimes(), modified: Self.start)
    }

    private static let start = Date(timeIntervalSince1970: 1_800_000_000)

    private func model() -> DiskModel {
        DiskModel(runner: runner, data: data.directory, temporaryDirectory: fake.directory)
    }

    /// A loaded model over the two-runtime report.
    private func loaded() async -> DiskModel {
        let m = model()
        await m.load()
        return m
    }

    private func write(_ report: Data, modified: Date) throws {
        try report.write(to: data.directory.lastDisk)
        try FileManager.default.setAttributes([.modificationDate: modified], ofItemAtPath: data.directory.lastDisk.path)
    }

    private func until(_ condition: () -> Bool) async throws {
        for _ in 0..<1_000 where !condition() {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(condition())
    }

    /// The disk report fixture with a second downloaded, idle runtime, iOS
    /// 26.0, listed after the first; `edit` changes it further.
    static func twoRuntimes(_ edit: (inout [String: Any]) -> Void = { _ in }) throws -> Data {
        try Fixture.edited("disk-report") { json in
            var sims = json["simulators"] as! [String: Any]
            var runtimes = sims["runtimes"] as! [[String: Any]]
            var other = runtimes[0]
            other["identifier"] = second
            other["runtime_identifier"] = "com.apple.CoreSimulator.SimRuntime.iOS-26-0"
            other["version"] = "26.0"
            other["build"] = "23A100"
            other["path"] = "/System/Library/AssetsV2/com_apple_MobileAsset_iOSSimulatorRuntime/y.asset"
            other["devices"] = [[String: Any]]()
            runtimes.append(other)
            sims["runtimes"] = runtimes
            sims["listed_bytes"] = 2 * 16_085_155_840
            json["simulators"] = sims
            edit(&json)
        }
    }

    private func offer(_ m: DiskModel, _ id: String) throws -> AreaActions.Offer {
        try #require(m.loadedReport.flatMap { AreaActions.orderedOffers($0).first { $0.id == id } })
    }

    private static let runtimeA = "simctl-runtime-delete \(first)"
    private static let runtimeB = "simctl-runtime-delete \(second)"

    // MARK: - Which rows can be checked

    @Test("a row is checkable exactly when it has an offer; the checks run in the rows' order")
    func rowOrder() async throws {
        let m = await loaded()
        let report = try #require(m.loadedReport)
        let rows = DiskSummary.make(report, scan: nil, summary: nil, included: [], now: .now).rows
        #expect(AreaActions.orderedOffers(report) == rows.compactMap(\.offer), "the rows and the run order cannot disagree")
        for row in rows {
            #expect((row.offer.map(m.canCheck) ?? false) == (row.offer != nil), "\(row.id)")
        }
        #expect(m.checked.isEmpty, "unticked by default")
        #expect(!m.canFreeChecked, "nothing checked")

        // Checked in reverse, listed in the rows' order.
        for id in [Self.runtimeB, Self.runtimeA, "brew-cleanup"] { m.toggle(try offer(m, id)) }
        #expect(m.checkedOffers.map(\.id) == ["brew-cleanup", Self.runtimeA, Self.runtimeB])
        #expect(m.canFreeChecked)
        m.toggle(try offer(m, "brew-cleanup"))
        #expect(m.checkedOffers.map(\.id) == [Self.runtimeA, Self.runtimeB], "a second toggle unchecks")
    }

    @Test("a conda environment and its install's package cache checked together run the removal first")
    func condaOrder() async throws {
        let m = await loaded()
        let clean = try offer(m, "conda-clean /opt/miniconda3")
        let ml = try offer(m, "conda-env-remove /opt/miniconda3/envs/ml")
        let report = try #require(m.loadedReport)
        let rows = AreaActions.orderedOffers(report).map(\.id)
        #expect(try #require(rows.firstIndex(of: clean.id)) < (try #require(rows.firstIndex(of: ml.id))), "the cache's row is above")

        m.toggle(clean)
        m.toggle(ml)
        m.toggle(try offer(m, Self.runtimeA))
        #expect(m.checkedOffers.map(\.id) == [ml.id, Self.runtimeA, clean.id])
        #expect(m.checkedOffers.map(\.listed).map(\.action) == [.condaEnvRemove, .simctlRuntimeDelete, .condaClean])

        // Stable among themselves: two cleans keep their rows' order, after every removal.
        let a = AreaActions.Offer(action: .condaClean, identifier: "/opt/a", name: "/opt/a", bytes: 1)
        let b = AreaActions.Offer(action: .condaClean, identifier: "/opt/b", name: "/opt/b", bytes: 1)
        let env = AreaActions.Offer(action: .condaEnvRemove, identifier: "/opt/b/envs/x", name: "x", bytes: 1)
        let brew = AreaActions.Offer(action: .brewCleanup, identifier: nil, name: "Homebrew", bytes: 1)
        #expect(AreaActions.runOrder([a, brew, b, env]) == [brew, env, a, b])
    }

    @Test("rows without an offer, a package removal, and every row during a scan or while a sheet is open cannot be checked; the button needs a check and the view free to act")
    func refusedChecks() async throws {
        let m = await loaded()
        let a = try offer(m, Self.runtimeA)

        // No offer: an id no row offers, or a booted runtime.
        let stranger = AreaActions.Offer(action: .simctlRuntimeDelete, identifier: "nowhere", name: "iOS 1.0", bytes: 1)
        #expect(!m.canCheck(stranger))
        m.toggle(stranger)
        #expect(m.checked.isEmpty)

        // Package removals never join a list.
        let wget = AreaActions.Offer(action: .brewUninstall, identifier: "wget", name: "wget", bytes: 1, kegs: ["1.24.5"])
        let orphans = AreaActions.Offer(action: .brewUninstallOrphans, identifier: nil, name: "unneeded", bytes: 1, names: ["libidn2"])
        for removal in [wget, orphans] {
            #expect(!removal.joinsList && !m.canCheck(removal))
            m.toggle(removal)
        }
        #expect(m.checked.isEmpty)

        // During a scan.
        let scan = Task { await m.scanDisk() }
        try await until { m.scanning }
        #expect(!m.canCheck(a))
        m.toggle(a)
        #expect(m.checked.isEmpty)
        try fake.open("disk-go")
        await scan.value
        m.toggle(a)
        #expect(m.checked == [a.id])
        #expect(m.canFreeChecked)

        // While a sheet is open: no new check, no change to one, no button.
        try fake.put("free-preview.gated", Data())
        let brew = try offer(m, "brew-cleanup")
        let preview = Task { await m.previewAction(brew) }
        try await until { m.freeSheet != nil }
        #expect(!m.canCheck(a) && !m.canFreeChecked)
        m.toggle(a)
        m.toggle(try offer(m, Self.runtimeB))
        #expect(m.checked == [a.id])
        m.dismissFree()
        await preview.value
        #expect(m.canCheck(a) && m.canFreeChecked)

        // Without the CLI nothing is checkable.
        let none = DiskModel(runner: nil, data: data.directory)
        await none.load()
        #expect(!none.canCheck(a))
    }

    @Test("checks of rows no longer offered are pruned when the report changes, to none or unreadable included; a check survives a Measure while its row is offered")
    func pruned() async throws {
        let m = await loaded()
        m.toggle(try offer(m, Self.runtimeA))
        m.toggle(try offer(m, Self.runtimeB))
        m.toggle(try offer(m, "brew-cleanup"))

        // A Measure that still offers them keeps them.
        try fake.open("disk-go")
        await m.scanDisk()
        #expect(m.checked == [Self.runtimeA, Self.runtimeB, "brew-cleanup"])

        // A new report where the second runtime booted: its check goes.
        try write(Self.twoRuntimes { json in
            var sims = json["simulators"] as! [String: Any]
            var runtimes = sims["runtimes"] as! [[String: Any]]
            runtimes[1]["booted"] = true
            sims["runtimes"] = runtimes
            json["simulators"] = sims
        }, modified: Self.start.addingTimeInterval(60))
        await m.load()
        #expect(m.checked == [Self.runtimeA, "brew-cleanup"])
        #expect(m.checkedOffers.map(\.id) == ["brew-cleanup", Self.runtimeA])

        // Unreadable: nothing is offered, nothing stays checked.
        try write(Fixture.edited("disk-report") { $0["schema_version"] = 1 }, modified: Self.start.addingTimeInterval(120))
        await m.load()
        #expect(m.checked.isEmpty)

        // No report.
        try write(Self.twoRuntimes(), modified: Self.start.addingTimeInterval(180))
        await m.load()
        m.toggle(try offer(m, Self.runtimeA))
        #expect(m.checked == [Self.runtimeA])
        try FileManager.default.removeItem(at: data.directory.lastDisk)
        await m.load()
        #expect(m.report == .none && m.checked.isEmpty && !m.canFreeChecked)
    }

    // MARK: - The checked run

    /// One action of a `disk free` report: a runtime's deletion.
    struct Line {
        var identifier: String
        var result: [String: Any]
        var estimate: UInt64? = nil
        var output: String? = nil
    }

    /// A `disk free` report of runtime deletions, from the run fixture.
    static func freeReport(dryRun: Bool, _ lines: [Line]) throws -> Data {
        try Fixture.edited("free-report") { json in
            let template = (json["actions"] as! [[String: Any]])[0]
            json["dry_run"] = dryRun
            if dryRun {
                json["journal"] = nil
                json["last_scan"] = "unchanged"
                json["last_disk"] = "unchanged"
            } else {
                json["last_scan"] = "unchanged"
            }
            json["actions"] = lines.map { line in
                var action = template
                action["action"] = "simctl-runtime-delete"
                action["area"] = "simulator-runtimes"
                action["identifier"] = line.identifier
                action["command"] = ["/usr/bin/xcrun", "simctl", "runtime", "delete", line.identifier]
                action["env"] = [String]()
                action["terminal_command"] = "/usr/bin/xcrun simctl runtime delete \(line.identifier)"
                action["result"] = line.result
                action["output"] = line.output
                if dryRun {
                    action["bytes_after"] = nil
                    var preview: [String: Any] = ["lines": ["Would delete \(line.identifier)"]]
                    preview["estimate_bytes"] = line.estimate
                    action["preview"] = preview
                }
                return action
            }
        }
    }

    private static let wouldRun: [String: Any] = ["status": "would-run"]
    private static let freed: [String: Any] = ["status": "freed"]

    /// A loaded model with both runtimes checked, and the fake's replies.
    private func bothChecked(preview: [Line], run: [Line]) async throws -> DiskModel {
        try fake.put("free-preview.json", Self.freeReport(dryRun: true, preview))
        try fake.put("free-report.json", Self.freeReport(dryRun: false, run))
        let m = await loaded()
        m.toggle(try offer(m, Self.runtimeB))
        m.toggle(try offer(m, Self.runtimeA))
        return m
    }

    private func starts() throws -> [String] {
        try fake.runs().filter { $0.event == "start" }.map(\.args)
    }

    private func lists() throws -> [String] {
        try fake.actionLists().map { String(decoding: $0, as: UTF8.self) }
    }

    private static func listJSON(_ ids: [String]) -> String {
        let actions = ids.map { #"{"action":"simctl-runtime-delete","identifier":"\#($0)"}"# }.joined(separator: ",")
        return #"{"actions":[\#(actions)],"schema_version":3}"#
    }

    @Test("two runtimes checked: one dry run with both, one run with both; each result goes to its row and the result shows no total")
    func freeBoth() async throws {
        let m = try await bothChecked(
            preview: [Line(identifier: Self.first, result: Self.wouldRun, estimate: 1 << 30),
                      Line(identifier: Self.second, result: Self.wouldRun, estimate: 2 << 30)],
            run: [Line(identifier: Self.first, result: Self.freed), Line(identifier: Self.second, result: Self.freed)]
        )
        await m.previewChecked()
        let sheet = try #require(m.freeSheet)
        #expect(sheet.offers.map(\.id) == [Self.runtimeA, Self.runtimeB])
        guard case .confirm(let previews) = sheet.stage else {
            Issue.record("no confirmation: \(sheet)")
            return
        }
        #expect(previews.map(\.identifier) == [Self.first, Self.second])
        #expect(sheet.question == "Free 2 areas?" && sheet.confirmTitle == "Free 2")
        #expect(sheet.totalText == "The tools estimate about \(ByteSize.human(3 << 30)) in all.")
        #expect(sheet.previewingText.hasPrefix("Checking 2 actions"))
        #expect(try starts().count == 1 && (try lists()) == [Self.listJSON([Self.first, Self.second])])

        try fake.open("free-go")
        await m.freeConfirmedActions()
        let after = try #require(m.freeSheet)
        guard case .result(let results) = after.stage else {
            Issue.record("no result: \(after)")
            return
        }
        #expect(after.offers.map(\.id) == [Self.runtimeA, Self.runtimeB])
        #expect(results.map(\.identifier) == [Self.first, Self.second])
        #expect(m.freeResults[Self.runtimeA] == results[0] && m.freeResults[Self.runtimeB] == results[1])
        #expect(after.totalText == nil, "ddc reports space per action, never a sum")
        let started = try starts()
        #expect(started.count == 2 && started[0].hasSuffix("--dry-run --json") && started[1].hasSuffix(".json --json"))
        #expect(try lists() == Array(repeating: Self.listJSON([Self.first, Self.second]), count: 2))
        #expect(m.checked.isEmpty, "the sheet's rows are unchecked after the run")
        await m.closeFree()
        #expect(m.freeSheet == nil)
    }

    @Test("a run report whose actions do not match the list is a failure, not a partial success")
    func mismatchedRun() async throws {
        let previews = [Line(identifier: Self.first, result: Self.wouldRun), Line(identifier: Self.second, result: Self.wouldRun)]
        let reports: [(String, [Line])] = [
            ("another order", [Line(identifier: Self.second, result: Self.freed), Line(identifier: Self.first, result: Self.freed)]),
            ("one short", [Line(identifier: Self.first, result: Self.freed)]),
            ("another identifier", [Line(identifier: Self.first, result: Self.freed), Line(identifier: "other", result: Self.freed)]),
        ]
        try fake.open("free-go")
        for (name, run) in reports {
            let m = try await bothChecked(preview: previews, run: run)
            await m.previewChecked()
            #expect(m.freeSheet?.confirmTitle == "Free 2", "\(name)")
            await m.freeConfirmedActions()
            #expect(m.freeSheet?.stage == .failed("The CLI's report does not match the confirmed actions."), "\(name)")
            #expect(m.freeResults.isEmpty, "\(name)")
            #expect(m.checked.isEmpty, "\(name): unchecked whatever the result")
            await m.closeFree()
        }
    }

    @Test("Stop after the first action leaves the second not run; a failed run unchecks the rows too")
    func stopAndFailure() async throws {
        let m = try await bothChecked(
            preview: [Line(identifier: Self.first, result: Self.wouldRun), Line(identifier: Self.second, result: Self.wouldRun)],
            run: []
        )
        try fake.put("free-stopped.json", Self.freeReport(dryRun: false, [
            Line(identifier: Self.first, result: ["status": "failed", "needs_admin": false], output: "stopped before the tool finished"),
            Line(identifier: Self.second, result: ["status": "not-run"]),
        ]))
        try fake.put("free.trap", Data())
        await m.previewChecked()
        let run = Task { await m.freeConfirmedActions() }
        try await until { (try? starts().count) == 2 }
        #expect(m.freeSheet?.runningText == "Freeing 2 areas… Stop ends the one that runs and leaves the rest not run.")
        await m.stopFree()
        await run.value
        guard case .result(let results)? = m.freeSheet?.stage else {
            Issue.record("no result: \(String(describing: m.freeSheet))")
            return
        }
        #expect(results.map(\.result) == [.failed(needsAdmin: false), .notRun])
        #expect(FreeWording.result(results[0], name: "iOS 27.0 (24A100)").hasPrefix("Stopped before simctl finished."))
        #expect(FreeWording.result(results[1], name: "iOS 26.0 (23A100)") == "Stopped before it started: nothing ran.")
        #expect(m.freeResults[Self.runtimeB]?.result == .notRun)
        #expect(m.checked.isEmpty)
        await m.closeFree()

        // A run that fails without a report (the lock is held) unchecks them too.
        let again = try await bothChecked(
            preview: [Line(identifier: Self.first, result: Self.wouldRun), Line(identifier: Self.second, result: Self.wouldRun)],
            run: []
        )
        try fake.put("free.exit", Data("75".utf8))
        try fake.open("free-go")
        await again.previewChecked()
        await again.freeConfirmedActions()
        #expect(again.freeSheet?.stage == .failed("Another scan or clean holds the lock, perhaps from Terminal. Try again when it ends."))
        #expect(again.checked.isEmpty && again.freeResults.isEmpty)
    }

    @Test("of two previews, one would run and one refused: the confirm runs a list of one, says Free 1 of 2, and the refused one gets no row result; with none, nothing is offered")
    func mixedPreviews() async throws {
        let m = try await bothChecked(
            preview: [Line(identifier: Self.first, result: ["status": "refused", "reason": "booted"], estimate: 1 << 30),
                      Line(identifier: Self.second, result: Self.wouldRun)],
            run: [Line(identifier: Self.second, result: Self.freed)]
        )
        await m.previewChecked()
        let sheet = try #require(m.freeSheet)
        guard case .confirm(let previews) = sheet.stage else {
            Issue.record("no confirmation: \(sheet)")
            return
        }
        #expect(sheet.confirmTitle == "Free 1 of 2")
        #expect(sheet.confirmed.map(\.id) == [Self.runtimeB])
        #expect(FreeWording.result(previews[0], name: "iOS 27.0 (24A100)") == "Not run: a simulator on it is running or not yet shut down.")
        #expect(sheet.totalText == nil, "the refused one's estimate is not counted, and the other gave none")

        // The confirm runs what the sheet showed would run, and nothing else.
        try fake.open("free-go")
        await m.freeConfirmedActions()
        guard case .result(let results)? = m.freeSheet?.stage else {
            Issue.record("no result: \(String(describing: m.freeSheet))")
            return
        }
        #expect(m.freeSheet?.offers.map(\.id) == [Self.runtimeB] && results.count == 1)
        #expect(try lists().last == Self.listJSON([Self.second]), "only the would-run action is sent")
        #expect(m.freeResults[Self.runtimeA] == nil && m.freeResults[Self.runtimeB] != nil)
        #expect(m.checked.isEmpty)
        await m.closeFree()

        // None would run: no confirm, no run.
        let none = try await bothChecked(
            preview: [Line(identifier: Self.first, result: ["status": "refused", "reason": "booted"]),
                      Line(identifier: Self.second, result: ["status": "failed", "needs_admin": false])],
            run: []
        )
        let before = try starts().count
        await none.previewChecked()
        #expect(none.freeSheet?.confirmTitle == nil && none.freeSheet?.confirmed.isEmpty == true)
        await none.freeConfirmedActions()
        #expect(try starts().count == before + 1, "the dry run only")
        #expect(none.checked == [Self.runtimeA, Self.runtimeB], "nothing ran: the checks stay")
        if case .confirm? = none.freeSheet?.stage {} else { Issue.record("the sheet left its confirm stage") }
    }

    @Test("the total says at least when a would-run preview gave no estimate; a list's refusal says the list")
    func totalAndRefusal() async throws {
        let m = try await bothChecked(
            preview: [Line(identifier: Self.first, result: Self.wouldRun, estimate: 1 << 30),
                      Line(identifier: Self.second, result: Self.wouldRun)],
            run: []
        )
        await m.previewChecked()
        #expect(m.freeSheet?.totalText == "The tools estimate at least \(ByteSize.human(1 << 30)) in all: not every one gave an estimate.")

        #expect(DiskModel.freeFailure(.refused, stderrTail: ["ddc disk free: an action named twice"], list: true)
            == "The CLI refused the list (exit 2): an action named twice.")
        #expect(DiskModel.freeFailure(.refused, stderrTail: [], list: false) == "The CLI refused the action (exit 2).")
    }

    @Test("a list of one keeps its offer's words")
    func listOfOne() async throws {
        let m = await loaded()
        let brew = try offer(m, "brew-cleanup")
        m.toggle(brew)
        try fake.put("free-preview.json", Fixture.data("free-preview"))
        await m.previewChecked()
        let sheet = try #require(m.freeSheet)
        #expect(sheet.question == brew.question && sheet.confirmTitle == brew.confirmTitle)
        #expect(sheet.previewingText == brew.previewingText && sheet.runningText == brew.runningText)
        #expect(sheet.totalText == nil)
    }
}
