import Foundation
import Testing
@testable import DDCKit

/// Story "Docker Desktop: what is inside its disk image, and what a clean
/// frees": `ddc disk docker --json` read strictly, the Review item's query,
/// and the sheet's "frees about" with and without its estimate. A fake
/// runner only; no docker runs.
@Suite("Docker Desktop's contents")
@MainActor
struct DockerContentsTests {
    let data: TempDataDirectory
    let defaults: TempDefaults

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
    }

    static func json(_ result: String, version: Int = 1) -> Data {
        Data(#"{"schema_version": \#(version), "ddc_version": "0.36.0", "result": \#(result)}"#.utf8)
    }

    static let listed = json(#"""
    {"status": "listed", "images": {"count": 18, "bytes": 2100000000}, "containers": {"count": 6, "bytes": 12300},
     "volumes": {"count": 20, "bytes": 1500000000}, "build_cache": {"count": 73, "bytes": 3400000000},
     "estimate": {"build_cache": 512004500, "dangling_images": 123400000, "bytes": 635404500}}
    """#)

    @Test("every status the CLI prints is read, an unknown one or another version is not")
    func decode() throws {
        let read = try DockerContents.read(Self.listed)
        guard case .listed(let listed) = read.result else { throw CancellationError() }
        #expect(listed.images == .init(count: 18, bytes: 2_100_000_000))
        #expect(listed.estimate == .init(buildCache: 512_004_500, danglingImages: 123_400_000, bytes: 635_404_500))
        for (json, result) in [
            (#"{"status": "not-installed"}"#, DockerContents.Result.notInstalled),
            (#"{"status": "not-ready", "detail": "Docker Desktop is not running or not answering"}"#, .notReady(detail: "Docker Desktop is not running or not answering")),
            (#"{"status": "failed", "detail": "x"}"#, .failed(detail: "x")),
            (#"{"status": "timed-out", "seconds": 60}"#, .timedOut(seconds: 60)),
            (#"{"status": "unreadable", "detail": "y"}"#, .unreadable(detail: "y")),
        ] {
            #expect(try DockerContents.read(Self.json(json)).result == result)
        }
        let unknown = #"{"status": "sleeping"}"#
        #expect(throws: DecodingError.self) { try DockerContents.read(Self.json(unknown)) }
        #expect(throws: ScanReport.ReadError.self) { try DockerContents.read(Self.json(unknown, version: 2)) }
        // A group docker could not size has no bytes; the estimate then none either.
        let partial = Self.json(#"""
        {"status": "listed", "images": {"count": 2}, "containers": {"count": 0, "bytes": 0}, "volumes": {"count": 0, "bytes": 0},
         "build_cache": {"count": 1, "bytes": 5}, "estimate": {"build_cache": 5}}
        """#)
        guard case .listed(let p) = try DockerContents.read(partial).result else { throw CancellationError() }
        #expect(p.images.bytes == nil && p.estimate.bytes == nil && p.estimate.danglingImages == nil)
    }

    @Test("the row asks once, keeps the answer, and asks again after a clean forgot it")
    func query() async throws {
        let runner = FakeRunner(scan: FakeRunner.done(Data()))
        await runner.reply(.dockerContents, FakeRunner.done(Self.listed))
        let model = DockerContentsModel(runner: runner, log: { _ in })
        #expect(model.state == .unknown && model.estimate == nil)
        await model.show()
        #expect(model.estimate == 635_404_500)
        await model.show()
        #expect(await runner.count(.dockerContents) == 1, "a result in is not asked again")
        #expect(await runner.arguments(of: .dockerContents) == [["disk", "docker", "--json"]])
        #expect(CLIRunner.Kind.dockerContents.hasLane, "the query runs beside the queue")
        model.forget()
        #expect(model.state == .unknown && model.estimate == nil)
        await model.show()
        #expect(await runner.count(.dockerContents) == 2)
        await model.again()
        #expect(await runner.count(.dockerContents) == 3, "Try Again asks whatever the last answer was")
    }

    @Test("when docker does not answer, the row says why and what to do")
    func unavailable() async throws {
        func state(_ result: String) async -> DockerContentsModel.State {
            let runner = FakeRunner(scan: FakeRunner.done(Data()))
            await runner.reply(.dockerContents, FakeRunner.done(Self.json(result)))
            let model = DockerContentsModel(runner: runner, log: { _ in })
            await model.show()
            #expect(model.estimate == nil)
            return model.state
        }
        #expect(await state(#"{"status": "not-ready", "detail": "Docker Desktop is not running or not answering"}"#)
            == .unavailable(reason: "Docker Desktop is not running or not answering.", advice: "Start Docker Desktop to see what is inside."))
        #expect(await state(#"{"status": "not-ready", "detail": "the desktop-linux context does not exist"}"#)
            == .unavailable(reason: "Docker did not answer: the desktop-linux context does not exist.", advice: "If you no longer use Docker Desktop, its folder is yours to delete in Finder."))
        #expect(await state(#"{"status": "not-installed"}"#) == .unavailable(reason: "Docker's command-line tool was not found.", advice: nil))
        #expect(await state(#"{"status": "timed-out", "seconds": 60}"#)
            == .unavailable(reason: "docker system df did not finish within 60 seconds.", advice: nil))
        #expect(await state(#"{"status": "failed", "detail": "Error response from daemon: busy."}"#)
            == .unavailable(reason: "docker system df failed: Error response from daemon: busy.", advice: nil))

        let crashed = FakeRunner(scan: FakeRunner.done(Data()))
        await crashed.reply(.dockerContents, CLIRunner.Result(outcome: .error(status: 1), stdout: Data(), stderrTail: ["ddc: cannot install the signal handler"]))
        let model = DockerContentsModel(runner: crashed, log: { _ in })
        await model.show()
        #expect(model.state == .unavailable(reason: "cannot install the signal handler", advice: nil))

        let none = DockerContentsModel(runner: nil, log: { _ in })
        await none.show()
        guard case .unavailable = none.state else { Issue.record("no CLI: \(none.state)"); return }
    }

    @Test("the row's lines read like docker system df, and say what a clean leaves")
    func lines() throws {
        guard case .listed(let listed) = try DockerContents.read(Self.listed).result else { throw CancellationError() }
        let lines = CleanWording.dockerLines(listed)
        #expect(lines[0] == "Inside: 18 images · 2.1GB unique to each, 6 containers · 12.3kB, 20 volumes · 1.5GB and 73 build-cache records · 3.4GB.")
        #expect(lines[1] == "A clean frees about 635.4MB: unused build cache 512MB and dangling images 123.4MB.")
        #expect(lines[2].contains("(docker image prune -a)") && lines[2].contains("(docker volume prune)") && lines[2].contains("(docker builder prune --all)"))
        #expect(lines[3].contains("do not add up to the folder's size"))
        #expect(CleanWording.dockerSize(0) == "0B")
        #expect(CleanWording.dockerSize(1_234_000_000) == "1.234GB")
        #expect(CleanWording.dockerSize(999) == "999B")
        let unknown = DockerContents.Listed(
            images: .init(count: 1, bytes: nil), containers: .init(count: 0, bytes: 0), volumes: .init(count: 0, bytes: 0),
            buildCache: .init(count: 0, bytes: 0), estimate: .init(buildCache: 0, danglingImages: nil, bytes: nil)
        )
        #expect(CleanWording.dockerLines(unknown)[0].hasPrefix("Inside: 1 image, size not known, "))
        #expect(CleanWording.dockerLines(unknown)[1].contains("not known"))
    }

    /// The groups report with Docker Desktop's item as finding 11.
    private func report() throws -> Data {
        let docker = try ReviewFixture.finding(8, [
            "path": "/Users/example/Library/Containers/com.docker.docker", "rule": "vm.docker-desktop", "removal": "native-only",
            "ino": 9, "bytes": 26_000_000_000, "reclaimable_bytes": 26_000_000_000,
        ])
        return try Fixture.editedFindings("scan-report-groups") { $0 += [docker] }
    }

    @Test("with docker's answer the item counts its estimate in frees about; without it, it stays not counted")
    func sheet() async throws {
        let snapshot = try ReviewFixture.snapshot(report: try report(), settingsIn: data)
        let runner = FakeRunner(scan: FakeRunner.done(Data()))
        await runner.reply(.dockerContents, FakeRunner.done(Self.listed))
        let review = makeReview(runner: runner, data: data, defaults: defaults) { snapshot }
        _ = review.open(ReviewRequest(group: nil, classes: [.safe, .rebuildable, .review], confirm: false))
        let docker = try ReviewFixture.id(try #require(review.items), "com.docker.docker")
        review.selection = [8, docker]
        let ollama = try #require(review.items?.row(8)).finding.reclaimableBytes

        // Not asked yet: not counted, with its size, and the total is a number.
        #expect(review.selectedReclaimable == ollama)
        review.openSheet()
        var sheet = try #require(review.sheet).confirmation
        #expect(sheet.reclaimable == ollama)
        #expect(sheet.notCounted.map(\.runtime) == ["Docker"])
        #expect(CleanWording.notCounted(sheet) != nil)

        // The query ends while the sheet shows: it counts at once.
        await review.docker.show()
        sheet = try #require(review.sheet).confirmation
        #expect(sheet.reclaimable == ollama + 635_404_500)
        #expect(sheet.notCounted.isEmpty && CleanWording.notCounted(sheet) == nil)
        #expect(review.selectedReclaimable == ollama + 635_404_500)
        // Skipped by the check: its estimate is left out as its size would be.
        #expect(sheet.reclaimable(excluding: [docker]) == ollama)

        // A clean forgets it: the next sheet does not count it until docker answers again.
        review.docker.forget()
        sheet = try #require(review.sheet).confirmation
        #expect(sheet.reclaimable == ollama && sheet.notCounted.map(\.runtime) == ["Docker"])
    }

    @Test("a failed query leaves the item not counted, with its reason on the row")
    func sheetWithoutAnswer() async throws {
        let snapshot = try ReviewFixture.snapshot(report: try report(), settingsIn: data)
        let runner = FakeRunner(scan: FakeRunner.done(Data()))
        await runner.reply(.dockerContents, FakeRunner.done(Self.json(#"{"status": "not-ready", "detail": "Docker Desktop is not running or not answering"}"#)))
        let review = makeReview(runner: runner, data: data, defaults: defaults) { snapshot }
        _ = review.open(ReviewRequest(group: nil, classes: [.safe, .rebuildable, .review], confirm: false))
        let docker = try ReviewFixture.id(try #require(review.items), "com.docker.docker")
        review.selection = [docker]
        await review.docker.show()
        review.openSheet()
        let sheet = try #require(review.sheet).confirmation
        #expect(sheet.reclaimable == 0)
        #expect(sheet.notCounted.map(\.bytes) == [26_000_000_000])
        guard case .unavailable(let reason, _) = review.docker.state else { Issue.record("\(review.docker.state)"); return }
        #expect(reason.contains("not running"))
    }

    @Test("Stop ends the query and says so, with Try Again; a late answer after a clean is dropped")
    func stopAndLateAnswer() async throws {
        let runner = DockerQueryRunner(answer: FakeRunner.done(Self.listed))
        let model = DockerContentsModel(runner: runner, log: { _ in })
        let first = Task { await model.show() }
        await untilHolds { await runner.pending == 1 }
        #expect(model.state == .reading)
        model.stop()
        await first.value
        #expect(model.state == .unavailable(reason: "Stopped.", advice: nil))
        #expect(await runner.cancels == 1)
        await model.show()
        #expect(await runner.runs == 1, "a stopped row waits for Try Again")

        let again = Task { await model.again() }
        await untilHolds { await runner.pending == 1 }
        await runner.release()
        await again.value
        #expect(model.estimate == 635_404_500)

        // A runner that answers anyway: what comes after a clean let go of it is dropped.
        let deaf = DockerQueryRunner(answer: FakeRunner.done(Self.listed), hearsCancel: false)
        let late = DockerContentsModel(runner: deaf, log: { _ in })
        let reading = Task { await late.show() }
        await untilHolds { await deaf.pending == 1 }
        let generation = late.generation
        late.forget()
        #expect(late.state == .unknown && late.generation == generation + 1)
        // The row shows again: the new query waits for the old one to end.
        let next = Task { await late.show() }
        await deaf.release()
        await reading.value
        await untilHolds { await deaf.pending == 1 }
        #expect(await deaf.runs == 2)
        await deaf.release()
        await next.value
        #expect(late.estimate == 635_404_500)
    }

    @Test("closing the window lets go of docker's answer: it is asked afresh next time")
    func windowClosed() async throws {
        let snapshot = try ReviewFixture.snapshot(report: try report(), settingsIn: data)
        let runner = FakeRunner(scan: FakeRunner.done(Data()))
        await runner.reply(.dockerContents, FakeRunner.done(Self.listed))
        let review = makeReview(runner: runner, data: data, defaults: defaults) { snapshot }
        _ = review.open(ReviewRequest(group: nil, classes: [.safe, .rebuildable, .review], confirm: false))
        await review.docker.show()
        #expect(review.docker.estimate != nil)
        let generation = review.docker.generation
        review.windowClosed()
        #expect(review.docker.state == .unknown && review.docker.generation == generation + 1)
    }

    @Test("the runtime's estimate counts once, however many of its folders are picked")
    func once() throws {
        let first = try ReviewFixture.finding(8, [
            "path": "/Users/example/Library/Containers/com.docker.docker", "rule": "vm.docker-desktop", "removal": "native-only",
            "ino": 9, "bytes": 26_000_000_000,
        ])
        let second = try ReviewFixture.finding(8, [
            "path": "/Users/example/.docker/desktop", "rule": "vm.docker-desktop", "removal": "native-only", "ino": 10, "bytes": 2_000_000_000,
        ])
        let items = try ReviewFixture.items(report: try Fixture.editedFindings("scan-report-groups") { $0 += [first, second] })
        let ids = items.rows.filter { $0.finding.rule == "vm.docker-desktop" }.map(\.id)
        #expect(ids.count == 2)
        var sheet = DDCKit.Confirmation(items, selection: Set(ids), deletePermanently: false)
        #expect(sheet.notCounted.map(\.bytes) == [28_000_000_000])
        sheet.setEstimate(100, rule: "vm.docker-desktop")
        #expect(sheet.reclaimable == 100)
        #expect(sheet.notCounted.isEmpty)
        sheet.setEstimate(nil, rule: "vm.docker-desktop")
        #expect(sheet.reclaimable == 0 && sheet.notCounted.map(\.bytes) == [28_000_000_000])
    }
}

/// Holds each `.dockerContents` run until `release()`; a cancel ends it as
/// cancelled unless `hearsCancel` is off.
actor DockerQueryRunner: CLIRunning {
    private(set) var runs = 0
    private(set) var cancels = 0
    private var waiting: [(ticket: CLIRunner.Ticket?, done: CheckedContinuation<CLIRunner.Result, Never>)] = []
    private let answer: CLIRunner.Result
    private let hearsCancel: Bool

    init(answer: CLIRunner.Result, hearsCancel: Bool = true) {
        self.answer = answer
        self.hearsCancel = hearsCancel
    }

    var pending: Int { waiting.count }

    func run(_ kind: CLIRunner.Kind, _ arguments: [String], options: CLIRunner.Options) async -> CLIRunner.Result {
        runs += 1
        return await withCheckedContinuation { waiting.append((options.ticket, $0)) }
    }

    func cancel(_ ticket: CLIRunner.Ticket) {
        cancels += 1
        guard hearsCancel, let index = waiting.firstIndex(where: { $0.ticket == ticket }) else { return }
        waiting.remove(at: index).done.resume(returning: CLIRunner.Result(outcome: .cancelled, stdout: Data(), stderrTail: []))
    }

    func release() {
        let all = waiting
        waiting = []
        for w in all { w.done.resume(returning: answer) }
    }
}

/// Yields until `condition` holds, a bounded number of times.
@MainActor
func untilHolds(_ condition: () async -> Bool) async {
    for _ in 0..<10_000 {
        if await condition() { return }
        await Task.yield()
    }
    Issue.record("the condition never held")
}
