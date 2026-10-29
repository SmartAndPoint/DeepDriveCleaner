import Foundation
import Testing
@testable import DDCKit

/// A runner whose runs wait until the test releases them, in the order asked.
actor GatedRunner: CLIRunning {
    private var waiting: [CheckedContinuation<Void, Never>] = []
    private(set) var cancelled: [CLIRunner.Ticket] = []

    func run(_ kind: CLIRunner.Kind, _ arguments: [String], options: CLIRunner.Options) async -> CLIRunner.Result {
        await withCheckedContinuation { waiting.append($0) }
        return CLIRunner.Result(outcome: .done, stdout: Data(), stderrTail: [])
    }

    func cancel(_ ticket: CLIRunner.Ticket) async {
        cancelled.append(ticket)
    }

    /// Lets the oldest waiting run return.
    func releaseOne() {
        waiting.removeFirst().resume()
    }

    /// Waits until `count` runs wait at the gate.
    func waitFor(_ count: Int) async throws {
        for _ in 0..<1_000 {
            if waiting.count >= count { return }
            try await Task.sleep(for: .milliseconds(5))
        }
        throw CancellationError()
    }
}

/// Story "Sparkle updates: check daily, Update and Relaunch, and the cask's
/// auto_updates": the runner's busy answer, which the postpone question and
/// the Update and Relaunch button read synchronously.
@Suite("Runner activity", .timeLimit(.minutes(1)))
@MainActor
struct RunnerActivityTests {
    let inner = GatedRunner()

    /// Waits until the activity has no run in flight.
    private func waitIdle(_ activity: RunnerActivity) async throws {
        for _ in 0..<1_000 {
            if !activity.busy { return }
            try await Task.sleep(for: .milliseconds(5))
        }
        throw CancellationError()
    }

    @Test("idle before any run")
    func idleAtStart() {
        let activity = RunnerActivity(inner)
        #expect(!activity.busy)
        #expect(activity.reason == nil)
    }

    @Test("a run counts from the moment it is asked for until it returns, then idle is told once")
    func countsWhileInFlight() async throws {
        let activity = RunnerActivity(inner)
        var idles = 0
        activity.onIdle = { idles += 1 }
        let run = Task { await activity.run(.scan, ["scan"]) }
        try await inner.waitFor(1)
        #expect(activity.busy)
        #expect(activity.reason == .scanning)
        #expect(activity.inFlight == [.scan: 1])

        await inner.releaseOne()
        _ = await run.value
        #expect(!activity.busy)
        #expect(activity.reason == nil)
        #expect(idles == 1)
    }

    @Test("idle comes only when the last of several runs returns")
    func idleAfterTheLast() async throws {
        let activity = RunnerActivity(inner)
        var idles = 0
        activity.onIdle = { idles += 1 }
        let scan = Task { await activity.run(.scan, ["scan"]) }
        try await inner.waitFor(1)
        let disk = Task { await activity.run(.disk, ["disk"]) }
        try await inner.waitFor(2)
        #expect(activity.inFlight == [.scan: 1, .disk: 1])

        await inner.releaseOne()
        _ = await scan.value
        #expect(activity.busy)
        #expect(idles == 0)

        await inner.releaseOne()
        _ = await disk.value
        #expect(!activity.busy)
        #expect(idles == 1)
    }

    @Test("a clean wins the reason over a scan beside it, and the scan's shows after it")
    func cleanWins() async throws {
        let activity = RunnerActivity(inner)
        let clean = Task { await activity.run(.clean, ["clean"]) }
        try await inner.waitFor(1)
        #expect(activity.reason == .cleaning)
        let disk = Task { await activity.run(.disk, ["disk"]) }
        try await inner.waitFor(2)
        #expect(activity.reason == .cleaning)

        await inner.releaseOne()
        _ = await clean.value
        #expect(activity.reason == .scanning)
        await inner.releaseOne()
        _ = await disk.value
        #expect(activity.reason == nil)
    }

    @Test("the dry runs before a clean or a free give the clean's reason",
          arguments: [CLIRunner.Kind.dryRun, .diskFree, .diskFreeDryRun])
    func cleanKinds(kind: CLIRunner.Kind) async throws {
        let activity = RunnerActivity(inner)
        let run = Task { await activity.run(kind, []) }
        try await inner.waitFor(1)
        #expect(activity.reason == .cleaning)
        await inner.releaseOne()
        _ = await run.value
    }

    @Test("two scans that join count each, until both return")
    func joinedRunsCountEach() async throws {
        let activity = RunnerActivity(inner)
        let first = Task { await activity.run(.scan, ["scan"]) }
        let second = Task { await activity.run(.scan, ["scan"]) }
        try await inner.waitFor(2)
        #expect(activity.inFlight == [.scan: 2])
        await inner.releaseOne()
        _ = await first.value
        #expect(activity.inFlight == [.scan: 1])
        await inner.releaseOne()
        _ = await second.value
        #expect(!activity.busy)
    }

    @Test("a caller cancelled while its run waits still counts it until the run returns")
    func cancelledCaller() async throws {
        let activity = RunnerActivity(inner)
        var idles = 0
        activity.onIdle = { idles += 1 }
        let run = Task { await activity.run(.backgroundScan, []) }
        try await inner.waitFor(1)
        run.cancel()
        #expect(activity.busy)

        await inner.releaseOne()
        _ = await run.value
        try await waitIdle(activity)
        #expect(idles == 1)
    }

    @Test("cancel reaches the runner it wraps")
    func cancelPassesThrough() async {
        let activity = RunnerActivity(inner)
        let ticket = CLIRunner.Ticket()
        await activity.cancel(ticket)
        #expect(await inner.cancelled == [ticket])
    }

    @Test("the popover's scan through it is counted, and the popover sees the same result")
    func modelsSeeTheWrappedRunner() async throws {
        let data = try TempDataDirectory()
        let defaults = try TempDefaults()
        let fake = FakeRunner(scan: FakeRunner.done(try Fixture.data("scan-report-groups")), blocks: true)
        let activity = RunnerActivity(fake)
        let model = try makeModel(runner: activity, data: data, defaults: defaults)
        let scan = Task { await model.scanNow() }
        try await fake.waitForScan()
        #expect(activity.reason == .scanning)
        await fake.finishScan()
        await scan.value
        #expect(!activity.busy)
        #expect(model.loadedReport != nil)
    }
}
