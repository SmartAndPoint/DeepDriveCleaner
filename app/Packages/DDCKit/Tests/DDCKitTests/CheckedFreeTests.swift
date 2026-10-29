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
}
