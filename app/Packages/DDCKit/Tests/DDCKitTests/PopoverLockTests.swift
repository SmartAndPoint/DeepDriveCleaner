import Foundation
import Testing

@testable import DDCKit

/// Scan now meets the lock `busyScans` times, then scans; `lock status`
/// names nobody.
actor BusyRunner: CLIRunning {
    private(set) var scans = 0
    private(set) var stops: [[String]] = []
    private let busyScans: Int

    init(busyScans: Int = 1) {
        self.busyScans = busyScans
    }

    func run(_ kind: CLIRunner.Kind, _ arguments: [String], options: CLIRunner.Options) async -> CLIRunner.Result {
        switch kind {
        case .scan:
            scans += 1
            if scans <= busyScans {
                return CLIRunner.Result(outcome: .busy, stdout: (try? Fixture.data("error-busy")) ?? Data(), stderrTail: [])
            }
            return FakeRunner.done((try? Fixture.data("scan-report")) ?? Data())
        case .lockStatus:
            return FakeRunner.done(LockHoldersTests.status([]))
        case .lockStop:
            stops.append(arguments)
            return FakeRunner.done(Data())
        default:
            return FakeRunner.done(Data())
        }
    }

    func cancel(_ ticket: CLIRunner.Ticket) async {}
}

@Suite("Popover: who holds the lock", .timeLimit(.minutes(1)))
@MainActor
struct PopoverLockTests {
    let data: TempDataDirectory
    let defaults: TempDefaults

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
    }

    private func until(_ condition: () -> Bool) async throws {
        for _ in 0..<500 where !condition() { try await Task.sleep(for: .milliseconds(10)) }
    }

    @Test("Scan now that meets the lock names the holder; Stop runs lock stop, and once it is gone the panel and notice go")
    func scanBusyThenStop() async throws {
        let runner = BusyRunner()
        let model = try makeModel(runner: runner, data: data, defaults: defaults)
        await model.scanNow()
        let busy = try #require(model.lockBusy)
        #expect(busy.purpose == .scanning)
        #expect(busy.holders.map(\.pid) == [4202])
        #expect(model.notices.contains { $0.id == "busy" })
        model.lockSleep = { _ in }
        model.stopLockHolder(busy.holders[0].id)
        try await until { model.lockBusy == nil }
        #expect(await runner.stops == [["lock", "stop", "--pid", "4202", "--started", "1792404120000000"]])
        #expect(!model.notices.contains { $0.id == "busy" })
        #expect(await runner.scans == 1, "Stop starts no scan")
    }

    @Test("Wait scans once the lock is free")
    func waitScans() async throws {
        let runner = BusyRunner()
        let model = try makeModel(runner: runner, data: data, defaults: defaults)
        await model.scanNow()
        model.lockSleep = { _ in }
        model.waitForLockThenScan()
        #expect(model.lockBusy?.waiting == true)
        for _ in 0..<500 where await runner.scans < 2 { try await Task.sleep(for: .milliseconds(10)) }
        #expect(await runner.scans == 2)
        try await until { model.lockBusy == nil }
        #expect(model.lockBusy == nil)
    }

    @Test("a scan Wait started that meets the lock again waits again")
    func waitsAgain() async throws {
        let runner = BusyRunner(busyScans: 2)
        let model = try makeModel(runner: runner, data: data, defaults: defaults)
        await model.scanNow()
        let sleeps = Sleeps()
        model.lockSleep = { try await sleeps.sleep($0) }
        model.waitForLockThenScan()
        for _ in 0..<500 where await runner.scans < 3 { try await Task.sleep(for: .milliseconds(10)) }
        #expect(await runner.scans == 3)
        #expect(sleeps.taken.contains(LockHolders.retryInterval), "the second wait began with the retry interval")
    }

    @Test("dismissing the busy notice ends the Wait and drops the holders")
    func dismissEndsWait() async throws {
        let runner = BusyRunner()
        let model = try makeModel(runner: runner, data: data, defaults: defaults)
        await model.scanNow()
        model.lockSleep = { _ in try await Task.sleep(for: .seconds(60)) }
        model.waitForLockThenScan()
        #expect(model.lockBusy?.waiting == true)
        let notice = try #require(model.notices.first { $0.id == "busy" })
        model.dismiss(notice)
        #expect(model.lockBusy == nil)
        try await Task.sleep(for: .milliseconds(100))
        #expect(await runner.scans == 1)
    }
}
