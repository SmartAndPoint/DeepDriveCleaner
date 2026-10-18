import Foundation
import Testing
@testable import DDCKit

// A broken join or order waits on a gate that never opens: fail, do not hang.
@Suite("CLIRunner", .timeLimit(.minutes(1)))
struct CLIRunnerTests {
    @Test("two scan requests while a scan runs start one process")
    func joinedScans() async throws {
        let fake = try FakeCLI()
        let runner = CLIRunner(executable: fake.executable)

        async let first = runner.run(.scan, ["wait", "scan"])
        try await fake.waitForStart("wait scan")
        async let second = runner.run(.scan, ["wait", "other"])
        async let third = runner.run(.scan, ["wait", "other"])
        try fake.open("scan")
        let outcomes = await [first.outcome, second.outcome, third.outcome]

        #expect(outcomes == [.done, .done, .done])
        #expect(try fake.runs().filter { $0.event == "start" }.count == 1)
    }

    @Test("a clean requested during a scan starts only after the scan ends")
    func cleanWaitsForScan() async throws {
        let fake = try FakeCLI()
        let runner = CLIRunner(executable: fake.executable)

        async let scan = runner.run(.scan, ["wait", "scan"])
        try await fake.waitForStart("wait scan")
        async let clean = runner.run(.clean, ["exit", "0"])
        try fake.open("scan")
        _ = await (scan, clean)

        let order = try fake.runs().map { "\($0.event) \($0.args)" }
        #expect(order == ["start wait scan", "end wait scan", "start exit 0", "end exit 0"])
    }

    @Test("two CLI processes never run at once")
    func runsNeverOverlap() async throws {
        let fake = try FakeCLI()
        let runner = CLIRunner(executable: fake.executable)
        try fake.open("go")

        await withTaskGroup(of: Void.self) { group in
            for kind in [CLIRunner.Kind.clean, .rules, .scan, .dryRun, .clean, .rules, .scan] {
                group.addTask { _ = await runner.run(kind, ["wait", "go"]) }
            }
        }

        let runs = try fake.runs()
        #expect(runs.count >= 4 && runs.count.isMultiple(of: 2))
        for pair in stride(from: 0, to: runs.count, by: 2) {
            #expect(runs[pair].event == "start")
            #expect(runs[pair + 1].event == "end" && runs[pair + 1].pid == runs[pair].pid)
        }
    }

    @Test("1 MiB of stderr, then 1 MiB of stdout, finishes without a deadlock")
    func bothPipesDrained() async throws {
        let fake = try FakeCLI()
        let runner = CLIRunner(executable: fake.executable)
        let lines = Counter()

        let result = await runner.run(.scan, ["flood"]) { _ in lines.increment() }

        #expect(result.outcome == .done)
        #expect(lines.count == FakeCLI.floodLines)
        #expect(result.stdout.count == FakeCLI.floodLines * 64)
        #expect(result.stderrTail.count == CLIRunner.tailLines)
    }

    @Test("exit statuses map to outcomes", arguments: [
        (Int32(0), RunOutcome.done),
        (1, .error(status: 1)),
        (2, .refused),
        (75, .busy),
    ])
    func exitStatuses(status: Int32, outcome: RunOutcome) async throws {
        let fake = try FakeCLI()
        let result = await CLIRunner(executable: fake.executable).run(.clean, ["exit", "\(status)"])
        #expect(result.outcome == outcome)
    }

    @Test("cancel sends SIGTERM to the running CLI")
    func cancel() async throws {
        let fake = try FakeCLI()
        let runner = CLIRunner(executable: fake.executable)

        async let scan = runner.run(.scan, ["wait", "never"])
        try await fake.waitForStart("wait never")
        await runner.cancel()

        #expect(await scan.outcome == .cancelled)
    }

    @Test("a signal nobody sent is a crash, not a cancel")
    func crash() async throws {
        let fake = try FakeCLI()
        let result = await CLIRunner(executable: fake.executable).run(.scan, ["crash"])
        #expect(result.outcome == .crashed(signal: SIGABRT))
    }

    @Test("a missing executable is reported, not thrown")
    func notLaunched() async {
        let missing = FileManager.default.temporaryDirectory.appending(path: "ddckit-missing-\(UUID().uuidString)")
        let result = await CLIRunner(executable: missing).run(.scan, [])
        guard case .notLaunched = result.outcome else {
            Issue.record("expected notLaunched, got \(result.outcome)")
            return
        }
    }
}
