import Foundation
import Synchronization
import Testing

@testable import DDCKit

/// A runner that answers `lock status` from a list of replies, one per call,
/// and records every call.
actor LockRunner: CLIRunning {
    private(set) var calls: [(CLIRunner.Kind, [String])] = []
    private var statuses: [Data]
    private let stopOutcome: RunOutcome

    init(statuses: [Data], stopOutcome: RunOutcome = .done) {
        self.statuses = statuses
        self.stopOutcome = stopOutcome
    }

    func run(_ kind: CLIRunner.Kind, _ arguments: [String], options: CLIRunner.Options) async -> CLIRunner.Result {
        calls.append((kind, arguments))
        switch kind {
        case .lockStatus:
            let data = statuses.isEmpty ? Data() : statuses.removeFirst()
            return FakeRunner.done(data)
        case .lockStop:
            return CLIRunner.Result(outcome: stopOutcome, stdout: Data(), stderrTail: [])
        default:
            return FakeRunner.done(Data())
        }
    }

    func cancel(_ ticket: CLIRunner.Ticket) async {}

    func kinds() -> [CLIRunner.Kind] { calls.map(\.0) }
}

/// Sleeps that return at once and are counted.
final class Sleeps: Sendable {
    let log = Mutex<[Duration]>([])
    var taken: [Duration] { log.withLock { $0 } }
    func sleep(_ d: Duration) async throws { log.withLock { $0.append(d) } }
}

@Suite("The lock's holders")
struct LockHoldersTests {
    static func status(_ holders: [String]) -> Data {
        Data(#"{"schema_version":1,"holders":[\#(holders.joined(separator: ","))]}"#.utf8)
    }

    static func holder(pid: Int, mode: String, command: String = "scan", extra: String = "") -> String {
        #"{"pid":\#(pid),"start_us":1792404000000000,"started":"2026-10-19T10:00:00Z","command":"\#(command)","mode":"\#(mode)","origin":"terminal"\#(extra)}"#
    }

    @Test("the CLI's shapes decode strictly: the fixtures, another version refused, an unknown command refused")
    func decodes() throws {
        let status = try LockStatus.read(Fixture.data("lock-status"))
        #expect(status.holders.map(\.pid) == [4101, 4202])
        #expect(status.holders[1].phase == .cleaning && status.holders[1].progress == .init(done: 3, total: 8))
        let busy = try BusyReport.read(Fixture.data("error-busy"))
        #expect(busy.holders.map(\.command) == [.itemsClean])
        #expect(throws: (any Error).self) { try LockStatus.read(Data(#"{"schema_version":2,"holders":[]}"#.utf8)) }
        #expect(throws: (any Error).self) { try LockStatus.read(Self.status([Self.holder(pid: 1, mode: "shared", command: "nap")])) }
        #expect(throws: BusyReport.ReadError.notBusy("capability-absent")) {
            try BusyReport.read(Data(#"{"error_schema_version":4,"error":{"code":"capability-absent","message":"m","simulated":false}}"#.utf8))
        }
        #expect(throws: BusyReport.ReadError.otherSchema(2)) {
            try BusyReport.read(Data(#"{"error_schema_version":2,"error":{"code":"busy","message":"m","simulated":false}}"#.utf8))
        }
        let none = try BusyReport.read(Data(#"{"error_schema_version":4,"error":{"code":"busy","message":"m","simulated":false}}"#.utf8))
        #expect(none.holders.isEmpty, "no holder named: no record, or no in-use-check")
    }

    @Test("a clean is blocked by any holder; the check and a scan only by an exclusive one")
    func blocking() throws {
        let holders = try LockStatus.read(Fixture.data("lock-status")).holders
        let shared = [holders[0]]
        #expect(LockPurpose.cleaning.blocked(by: shared))
        #expect(!LockPurpose.checking.blocked(by: shared))
        #expect(!LockPurpose.scanning.blocked(by: shared))
        for purpose in [LockPurpose.cleaning, .checking, .scanning] {
            #expect(purpose.blocked(by: holders), "an exclusive holder blocks \(purpose)")
            #expect(!purpose.blocked(by: []))
        }
    }

    @Test("the words name the holder, where it was started, its age and a clean's item, and what Stop does")
    func wording() throws {
        let holders = try LockStatus.read(Fixture.data("lock-status")).holders
        let now = try #require(Wording.date(rfc3339: "2026-10-19T10:05:00Z"))
        #expect(Wording.lockHolder(holders[0], now: now) == "A scan started from DeepDriveCleaner 5 min ago (pid 4101)")
        #expect(Wording.lockHolder(holders[1], now: now) == "A clean started from a terminal 3 min ago (pid 4202), on item 4 of 8")
        #expect(Wording.lockStopEffect(holders[1]) == "Stop ends it after this item.")
        let busy = try BusyReport.read(Fixture.data("error-busy")).holders[0]
        #expect(Wording.lockHolder(busy, now: now).hasSuffix(", re-checking its items"))
        #expect(Wording.lockStopEffect(busy) == "Stop ends it at once, before it removes anything.")
        #expect(Wording.lockStopEffect(holders[0]) == "Stop ends it at once.")
        #expect(Wording.lockStopping(holders[1]) == "Stopping: finishing the current item…")
        #expect(Wording.lockStopping(holders[0]) == "Stopping…", "a scan ends at once")
        #expect(Wording.lockWaitHint(.cleaning).contains("while this sheet stays open"))
        #expect(Wording.lockWaiting(.scanning) == "Waiting to scan…")
    }

    @Test("Stop runs lock stop with the shown pid and start; a refusal is false")
    func stop() async throws {
        let h = try LockStatus.read(Fixture.data("lock-status")).holders[1]
        let runner = LockRunner(statuses: [])
        #expect(await LockHolders(runner: runner).stop(h))
        let calls = await runner.calls
        #expect(calls.count == 1)
        #expect(calls[0].0 == .lockStop)
        #expect(calls[0].1 == ["lock", "stop", "--pid", "4202", "--started", "1792404120000000"])
        let refused = LockRunner(statuses: [], stopOutcome: .error(status: 2))
        #expect(!(await LockHolders(runner: refused).stop(h)))
    }

    @Test("Wait polls lock status until nothing blocks, then says to start")
    func waitPolls() async throws {
        let exclusive = Self.holder(pid: 4202, mode: "exclusive")
        let shared = Self.holder(pid: 4101, mode: "shared")
        let runner = LockRunner(statuses: [Self.status([exclusive]), Self.status([exclusive, shared]), Self.status([shared])])
        let sleeps = Sleeps()
        let wait = LockHolders(runner: runner, sleep: { try await sleeps.sleep($0) })
        #expect(await wait.waitUntilFree(for: .scanning, named: true))
        #expect(await runner.kinds() == [.lockStatus, .lockStatus, .lockStatus])
        #expect(sleeps.taken == [LockHolders.pollInterval, LockHolders.pollInterval])
    }

    @Test("with no holder named, or lock status unreadable, Wait tries again after the retry interval")
    func waitRetries() async throws {
        let sleeps = Sleeps()
        let runner = LockRunner(statuses: [Data("not json".utf8)])
        let wait = LockHolders(runner: runner, sleep: { try await sleeps.sleep($0) })
        #expect(await wait.waitUntilFree(for: .cleaning, named: false))
        #expect(await runner.kinds().isEmpty, "nothing to poll")
        #expect(await wait.waitUntilFree(for: .cleaning, named: true))
        #expect(sleeps.taken == [LockHolders.retryInterval, LockHolders.retryInterval])
    }

    @Test("after a run that met the lock again, Wait waits the retry interval before it reads lock status")
    func waitAgain() async throws {
        let sleeps = Sleeps()
        let runner = LockRunner(statuses: [Self.status([])])
        let wait = LockHolders(runner: runner, sleep: { try await sleeps.sleep($0) })
        #expect(await wait.waitUntilFree(for: .checking, named: true, again: true))
        #expect(sleeps.taken == [LockHolders.retryInterval])
        #expect(await runner.kinds() == [.lockStatus])
    }

    @Test("a Wait cancelled while lock status runs says not to start, though the lock is free")
    func waitCancelledDuringStatus() async throws {
        let runner = GatedStatusRunner()
        let wait = LockHolders(runner: runner, sleep: { _ in })
        let task = Task { await wait.waitUntilFree(for: .cleaning, named: true) }
        await runner.started()
        task.cancel()
        await runner.release()
        #expect(await task.value == false)
    }

    @Test("a cancelled Wait says not to start")
    func waitCancelled() async throws {
        let runner = LockRunner(statuses: Array(repeating: Self.status([Self.holder(pid: 4202, mode: "exclusive")]), count: 100))
        let wait = LockHolders(runner: runner, sleep: { _ in throw CancellationError() })
        #expect(!(await wait.waitUntilFree(for: .cleaning, named: true)))
        #expect(!(await wait.waitUntilFree(for: .cleaning, named: false)))
    }
}

/// Answers `lock status` with no holder, but only once the test releases
/// it: the Wait is cancelled while the run is in flight.
actor GatedStatusRunner: CLIRunning {
    private var waiting: CheckedContinuation<Void, Never>?
    private var arrived: CheckedContinuation<Void, Never>?
    private var inFlight = false

    func run(_ kind: CLIRunner.Kind, _ arguments: [String], options: CLIRunner.Options) async -> CLIRunner.Result {
        inFlight = true
        arrived?.resume()
        arrived = nil
        await withCheckedContinuation { waiting = $0 }
        return FakeRunner.done(LockHoldersTests.status([]))
    }

    func cancel(_ ticket: CLIRunner.Ticket) async {}

    /// Returns once the status run is in flight.
    func started() async {
        if inFlight { return }
        await withCheckedContinuation { arrived = $0 }
    }

    func release() {
        waiting?.resume()
        waiting = nil
    }
}
