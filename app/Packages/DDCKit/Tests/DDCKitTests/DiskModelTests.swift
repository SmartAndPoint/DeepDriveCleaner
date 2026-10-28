import Foundation
import Testing
@testable import DDCKit

/// The Whole disk view's model over the real runner and a fake CLI (story
/// "Whole disk in the app").
@MainActor
@Suite("Whole-disk model")
struct DiskModelTests {
    private let data: TempDataDirectory
    private let fake: FakeCLI

    init() throws {
        data = try TempDataDirectory()
        fake = try FakeCLI(lastDisk: data.directory.lastDisk)
        try fake.put("disk-report.json", Fixture.data("disk-report"))
    }

    private func model(runner: Bool = true) -> DiskModel {
        DiskModel(runner: runner ? CLIRunner(executable: fake.executable) : nil, data: data.directory)
    }

    private func write(_ fixture: String, modified: Date) throws {
        try Fixture.data(fixture).write(to: data.directory.lastDisk)
        try FileManager.default.setAttributes([.modificationDate: modified], ofItemAtPath: data.directory.lastDisk.path)
    }

    /// Polls until `condition` holds, for about ten seconds.
    private func until(_ condition: () -> Bool) async throws {
        for _ in 0..<1_000 where !condition() {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(condition())
    }

    private static let args = "disk --json --progress json"

    // MARK: - The report on disk

    @Test("no file is no report; another schema is unreadable and says so")
    func loadStates() async throws {
        let m = model()
        await m.load()
        #expect(m.report == .none)
        #expect(m.summary(scan: nil, summary: nil, included: []) == nil)

        try Fixture.edited("disk-report") { $0["schema_version"] = 2 }.write(to: data.directory.lastDisk)
        await m.load()
        #expect(m.report == .unreadable("It was written by another version of ddc (schema 2)."))
    }

    @Test("the file is read again only when its modification date changed")
    func reloadOnDate() async throws {
        let m = model()
        let first = Date(timeIntervalSince1970: 1_800_000_000)
        try write("disk-report", modified: first)
        await m.load()
        #expect(m.loadedReport?.cancelled == false)

        // Same date, other content: the view appearing again does not decode it.
        try write("disk-cancelled", modified: first)
        await m.load()
        #expect(m.loadedReport?.cancelled == false)

        try write("disk-cancelled", modified: first.addingTimeInterval(60))
        await m.load()
        #expect(m.loadedReport?.cancelled == true)
    }

    // MARK: - Scan whole disk

    @Test("a run shows its progress, then the report it printed and saved")
    func scan() async throws {
        // `ddc disk` sends no `files_expected` today; the model reads it if it does.
        try fake.put("disk-events.jsonl", Data("""
            {"schema_version":2,"event":"walk","step":"disk","files":40,"dirs":3,"bytes":4096,"files_expected":100}
            {"schema_version":2,"event":"walk","step":"scan","files":999,"dirs":3,"bytes":1}

            """.utf8))
        let m = model()
        await m.load()
        let run = Task { await m.scanDisk() }
        try await until { m.progress?.files == 40 }
        #expect(m.scanning)
        #expect(m.progress?.fraction == 0.4)
        #expect(m.progress?.bytes == 4096, "a walk of another step is not this run's")

        try fake.open("disk-go")
        await run.value
        #expect(!m.scanning)
        #expect(m.message == nil)
        #expect(m.loadedReport == (try DiskReport.read(Fixture.data("disk-report"))))
        #expect(FileManager.default.fileExists(atPath: data.directory.lastDisk.path))
        #expect(try fake.runs().filter { $0.event == "start" }.map(\.args) == [Self.args])
    }

    @Test("a second Scan whole disk joins the running one")
    func join() async throws {
        let m = model()
        let first = Task { await m.scanDisk() }
        try await fake.waitForStart(Self.args)
        let second = Task { await m.scanDisk() }
        try await Task.sleep(for: .milliseconds(50))
        try fake.open("disk-go")
        await first.value
        await second.value
        #expect(try fake.runs().filter { $0.event == "start" }.count == 1)
        #expect(m.loadedReport != nil)
    }

    @Test("Stop sends SIGTERM and the model shows the partial report the CLI printed")
    func stopShowsPartial() async throws {
        try fake.put("disk.trap", Data())
        try fake.put("disk-stopped.json", Fixture.data("disk-cancelled"))
        let m = model()
        let run = Task { await m.scanDisk() }
        try await fake.waitForStart(Self.args)
        await m.stop()
        await run.value

        #expect(m.message == nil)
        let report = try #require(m.loadedReport)
        #expect(report.cancelled)
        #expect(m.summary(scan: nil, summary: nil, included: [])?.stopped == true)
        #expect(m.summary(scan: nil, summary: nil, included: [])?.rows.contains { $0.size.hasPrefix("at least") } == true)
    }

    @Test("a stop with nothing on stdout keeps the report before it")
    func stopWithoutReport() async throws {
        try fake.put("disk.trap", Data())
        try write("disk-report", modified: Date(timeIntervalSince1970: 1_800_000_000))
        let m = model()
        await m.load()
        let run = Task { await m.scanDisk() }
        try await fake.waitForStart(Self.args)
        await m.stop()
        await run.value

        #expect(m.loadedReport?.cancelled == false)
        #expect(m.message == "Stopped before a report was written; the last one is shown.")
    }

    @Test("a failed run keeps the report before it and says why, in this view's words")
    func failure() async throws {
        try fake.put("disk.exit", Data("1".utf8))
        try fake.open("disk-go")
        try write("disk-report", modified: Date(timeIntervalSince1970: 1_800_000_000))
        let m = model()
        await m.load()
        await m.scanDisk()

        #expect(m.loadedReport != nil)
        #expect(m.message == "The whole-disk scan failed (exit 1).")
        #expect(DiskModel.failure(.error(status: 1), stderrTail: ["ddc: no startup volume"]) == "no startup volume")
    }

    @Test("a failed run reads the file again when it changed during the run")
    func failureReloadsAChangedFile() async throws {
        try fake.put("disk.exit", Data("1".utf8))
        let first = Date(timeIntervalSince1970: 1_800_000_000)
        try write("disk-report", modified: first)
        let m = model()
        await m.load()
        let run = Task { await m.scanDisk() }
        try await fake.waitForStart(Self.args)
        // `ddc disk` from a terminal finished meanwhile.
        try write("disk-cancelled", modified: first.addingTimeInterval(60))
        try fake.open("disk-go")
        await run.value

        #expect(m.loadedReport?.cancelled == true)
        #expect(m.message == "The whole-disk scan failed (exit 1).")
    }

    @Test("without the embedded CLI nothing runs and the view says so")
    func noRunner() async {
        let m = model(runner: false)
        #expect(!m.canScan)
        await m.scanDisk()
        #expect(m.message?.contains("embedded CLI is missing") == true)
    }
}
