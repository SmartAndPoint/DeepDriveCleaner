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

    @Test("progress events reach the line callback but stay out of the stderr tail")
    func eventsLeaveTheTail() async throws {
        let fake = try FakeCLI()
        let runner = CLIRunner(executable: fake.executable)
        let stderr = """
            ddc: warning: one
            {"schema_version":1,"event":"phase","phase":"preparing"}
            ddc: two

            """
        try fake.put("stderr.txt", Data(stderr.utf8))
        let lines = Lines()

        let result = await runner.run(.scan, ["stderr", fake.directory.appending(path: "stderr.txt").path]) { lines.append($0) }

        #expect(lines.all == ["ddc: warning: one", #"{"schema_version":1,"event":"phase","phase":"preparing"}"#, "ddc: two"])
        #expect(result.stderrTail == ["ddc: warning: one", "ddc: two"])
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

    @Test("stdout lines reach the callback while the run goes on, and all of stdout is still returned")
    func stdoutLines() async throws {
        let fake = try FakeCLI()
        let runner = CLIRunner(executable: fake.executable)
        try fake.put("dry-run.jsonl", Data("one\ntwo\nthree\n".utf8))
        try fake.open("dry-run.gated")
        try fake.put("list.json", Data("{}".utf8))
        let lines = Lines()
        let started = Counter()

        async let run = runner.run(
            .dryRun,
            ["clean", "--items", fake.directory.appending(path: "list.json").path, "--dry-run", "--json"],
            options: CLIRunner.Options(stdoutLine: { lines.append($0) }, onStart: { started.increment() })
        )
        try fake.open("line-0")
        for _ in 0..<500 where lines.all.isEmpty { try await Task.sleep(for: .milliseconds(10)) }
        #expect(lines.all == ["one"], "while the CLI waits for line-1")
        #expect(started.count == 1)
        try fake.open("line-1")
        try fake.open("line-2")
        let result = await run
        #expect(lines.all == ["one", "two", "three"])
        #expect(String(decoding: result.stdout, as: UTF8.self) == "one\ntwo\nthree\n")
    }

    @Test("cancelling a waiting run's ticket: it never starts, and the run ahead of it goes on")
    func cancelQueuedTicket() async throws {
        let fake = try FakeCLI()
        let runner = CLIRunner(executable: fake.executable)
        let ticket = CLIRunner.Ticket()

        async let scan = runner.run(.scan, ["wait", "scan"])
        try await fake.waitForStart("wait scan")
        async let clean = runner.run(.clean, ["exit", "0"], options: CLIRunner.Options(ticket: ticket))
        try await Task.sleep(for: .milliseconds(50))
        await runner.cancel(ticket)
        try fake.open("scan")

        #expect(await scan.outcome == .done)
        let cancelled = await clean
        #expect(cancelled.outcome == .cancelled && cancelled.stdout.isEmpty)
        #expect(try fake.runs().filter { $0.event == "start" }.map(\.args) == ["wait scan"])
    }

    @Test("cancelling a running run's ticket sends SIGTERM to it only")
    func cancelRunningTicket() async throws {
        let fake = try FakeCLI()
        let runner = CLIRunner(executable: fake.executable)
        let ticket = CLIRunner.Ticket()

        async let run = runner.run(.clean, ["wait", "never"], options: CLIRunner.Options(ticket: ticket))
        try await fake.waitForStart("wait never")
        await runner.cancel(CLIRunner.Ticket())  // another run's ticket: no effect
        try await Task.sleep(for: .milliseconds(50))
        #expect(try fake.runs().filter { $0.event == "end" }.isEmpty)
        await runner.cancel(ticket)
        #expect(await run.outcome == .cancelled)
    }

    // MARK: - The disk lane (story "Whole disk in the app")

    @Test("a disk run held at a gate delays neither a scan nor a clean")
    func diskBesideTheQueue() async throws {
        let fake = try FakeCLI()
        let runner = CLIRunner(executable: fake.executable)

        async let disk = runner.run(.disk, ["wait", "disk"])
        try await fake.waitForStart("wait disk")
        #expect(await runner.run(.scan, ["exit", "0"]).outcome == .done)
        #expect(await runner.run(.clean, ["exit", "1"]).outcome == .error(status: 1))
        #expect(!(try fake.runs().contains { $0.event == "end" && $0.args == "wait disk" }), "the disk run still runs")
        try fake.open("disk")
        #expect(await disk.outcome == .done)
    }

    @Test("a disk run does not wait for a scan of the queue")
    func diskDoesNotWait() async throws {
        let fake = try FakeCLI()
        let runner = CLIRunner(executable: fake.executable)

        async let scan = runner.run(.scan, ["wait", "scan"])
        try await fake.waitForStart("wait scan")
        #expect(await runner.run(.disk, ["exit", "0"]).outcome == .done)
        try fake.open("scan")
        #expect(await scan.outcome == .done)
    }

    @Test("a second disk request joins the running one")
    func joinedDisk() async throws {
        let fake = try FakeCLI()
        let runner = CLIRunner(executable: fake.executable)

        async let first = runner.run(.disk, ["wait", "disk"])
        try await fake.waitForStart("wait disk")
        async let second = runner.run(.disk, ["wait", "other"])
        try await Task.sleep(for: .milliseconds(50))
        try fake.open("disk")
        let outcomes = await [first.outcome, second.outcome]

        #expect(outcomes == [.done, .done])
        #expect(try fake.runs().filter { $0.event == "start" }.map(\.args) == ["wait disk"])
        #expect(await runner.run(.disk, ["exit", "0"]).outcome == .done, "the lane is free again")
    }

    @Test("with a disk run and a scan running at once, each ticket stops its own run only", arguments: [true, false])
    func ticketsWithTwoRuns(diskFirst: Bool) async throws {
        let fake = try FakeCLI()
        let runner = CLIRunner(executable: fake.executable)
        let scanTicket = CLIRunner.Ticket()
        let diskTicket = CLIRunner.Ticket()

        async let disk = runner.run(.disk, ["wait", "disk"], options: CLIRunner.Options(ticket: diskTicket))
        try await fake.waitForStart("wait disk")
        async let scan = runner.run(.scan, ["wait", "scan"], options: CLIRunner.Options(ticket: scanTicket))
        try await fake.waitForStart("wait scan")

        let (first, other) = diskFirst ? (diskTicket, "wait scan") : (scanTicket, "wait disk")
        await runner.cancel(first)
        try await Task.sleep(for: .milliseconds(50))
        #expect(!(try fake.runs().contains { $0.event == "end" && $0.args == other }), "the other run still runs")
        await runner.cancel(diskFirst ? scanTicket : diskTicket)
        #expect(await [disk.outcome, scan.outcome] == [.cancelled, .cancelled])
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
