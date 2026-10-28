import Foundation
import Testing
@testable import DDCKit

/// The Coding agents section (story "Coding agents in the Whole disk view"):
/// the report, its rows, and the model over the real runner and a fake CLI.
@Suite("Coding agents report")
struct AgentsReportTests {
    @Test("the fixture decodes, with a status, an entrypoint and a key the app does not know")
    func decodes() throws {
        let report = try AgentsReport.read(Fixture.data("agents-report"))
        #expect(report.sizes)
        #expect(report.harnesses.map(\.id) == [.claudeCode, .codex, .cursor, .opencode])
        let live = try #require(report.harnesses[0].sessions?.list.first)
        #expect(live.state == .live)
        #expect(live.status == "thinking-in-a-new-way")
        #expect(live.entrypoint == "sdk-future")
        #expect(report.harnesses[2].sessions == nil)

        let fast = try AgentsReport.read(Fixture.data("agents-fast"))
        #expect(!fast.sizes)
        #expect(fast.harnesses.allSatisfy { $0.bytes == nil && $0.folders.allSatisfy { $0.bytes == nil } })
    }

    @Test("another schema is refused by its version; an unknown enum value of ddc's own is refused")
    func strict() throws {
        #expect(throws: ScanReport.ReadError.otherSchema(2)) {
            try AgentsReport.read(Fixture.edited("agents-report") { $0["schema_version"] = 2 })
        }
        #expect(throws: (any Error).self) {
            try AgentsReport.read(Fixture.edited("agents-report") { report in
                var harnesses = report["harnesses"] as! [[String: Any]]
                harnesses[0]["id"] = "some-new-harness"
                report["harnesses"] = harnesses
            })
        }
    }
}

@Suite("Coding agents rows")
struct AgentsSummaryTests {
    private static let now = Date(timeIntervalSince1970: 1_793_196_000)  // 2026-10-28 14:00 UTC
    private static var utc: Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return calendar
    }

    private func summary(_ name: String, edit: ((inout [String: Any]) -> Void)? = nil) throws -> AgentsSummary {
        let data = try edit.map { try Fixture.edited(name, $0) } ?? Fixture.data(name)
        return AgentsSummary.make(
            try AgentsReport.read(data), home: "/Users/example", now: Self.now,
            calendar: Self.utc, locale: Locale(identifier: "en_GB")
        )
    }

    @Test("a row per harness, its folders, its live sessions nested, and the transcripts line")
    func rows() throws {
        let s = try summary("agents-report")
        #expect(s.rows.filter { $0.kind == .harness }.map(\.title) == ["Claude Code", "Codex", "Cursor", "opencode"])
        let claude = try #require(s.rows.first { $0.id == "claude-code" })
        #expect(claude.size == "6.0 GiB")
        #expect(claude.note == "Runs: 2 CLI processes")
        #expect(s.rows.contains { $0.kind == .folder && $0.title == "~/.claude/projects" && $0.size == "5.0 GiB" })

        // Only the live session; stale and unknown ones are not listed.
        let sessions = s.rows.filter { $0.kind == .session }
        #expect(sessions.count == 2)
        #expect(sessions[0].title == "acme-web")
        #expect(sessions[0].note == "Fix the parser · thinking-in-a-new-way · started today at 13:41")
        #expect(sessions[1].note == "open in Codex · started 2026-10-28 09:41:07")

        let transcripts = try #require(s.rows.first { $0.id == "claude-code/transcripts" })
        #expect(transcripts.title == "42 transcripts of ended sessions")
        #expect(transcripts.size == "1.0 GiB")
        #expect(s.rows.first { $0.id == "codex/transcripts" }?.title == "1 transcript of ended sessions")

        // The order: each harness's own rows under it.
        let ids = s.rows.map(\.id)
        #expect(try #require(ids.firstIndex(of: "claude-code/transcripts")) < (try #require(ids.firstIndex(of: "codex"))))
        #expect(s.rows.first { $0.id == "codex" }?.note == "Runs: the app server")
        #expect(s.rows.first { $0.id == "cursor" }?.note == "Runs: the app")
        let opencode = try #require(s.rows.first { $0.id == "opencode" })
        #expect(opencode.note == "Not running")
        #expect(opencode.size == "at least 100.0 MiB")
        #expect(s.rows.contains { $0.kind == .folder && $0.note == "2 unreadable" })
    }

    @Test("before the sizes, every size reads empty and the sessions are there")
    func fast() throws {
        let s = try summary("agents-fast")
        #expect(!s.sizes)
        #expect(s.rows.allSatisfy { $0.size.isEmpty })
        #expect(s.rows.filter { $0.kind == .session }.count == 2)
        #expect(s.rows.first { $0.id == "claude-code/transcripts" }?.title == "42 transcripts of ended sessions")
    }

    @Test("unknown records are one line instead of the sessions")
    func unknownRecords() throws {
        let s = try summary("agents-report") { report in
            var harnesses = report["harnesses"] as! [[String: Any]]
            harnesses[0]["sessions"] = [
                "records": "unknown", "reason": "unparsed", "list": [] as [Any],
                "transcripts": ["count": 3, "bytes": 1024, "skipped": false],
            ] as [String: Any]
            report["harnesses"] = harnesses
        }
        let line = try #require(s.rows.first { $0.kind == .records })
        #expect(line.title == "ddc cannot read Claude Code's session records")
        #expect(!s.rows.contains { $0.kind == .session && $0.id.hasPrefix("claude-code/") })
    }

    @Test("a harness under a skip reads skipped, and its transcripts as not read")
    func skipped() throws {
        let s = try summary("agents-report") { report in
            var harnesses = report["harnesses"] as! [[String: Any]]
            harnesses[0]["folders"] = [["path": "/Users/example/.claude", "nested": false, "skipped": true, "unreadable": 0]]
            harnesses[0].removeValue(forKey: "bytes")
            harnesses[0]["sessions"] = [
                "records": "unknown", "reason": "skipped", "list": [] as [Any],
                "transcripts": ["skipped": true],
            ] as [String: Any]
            report["harnesses"] = harnesses
        }
        let claude = try #require(s.rows.first { $0.id == "claude-code" })
        #expect(claude.size == "skipped")
        #expect(claude.note == "Skipped in Settings")
        #expect(s.rows.first { $0.id == "claude-code/transcripts" }?.title == "Transcripts not read")
        #expect(s.rows.first { $0.kind == .records }?.note == "Its folder is skipped in Settings.")
    }

    @Test("text a harness wrote is escaped")
    func escaped() throws {
        let s = try summary("agents-report") { report in
            var harnesses = report["harnesses"] as! [[String: Any]]
            var sessions = harnesses[0]["sessions"] as! [String: Any]
            var list = sessions["list"] as! [[String: Any]]
            list[0]["cwd"] = "/Users/example/Projects/cod\u{202E}fdp.exe"
            sessions["list"] = list
            harnesses[0]["sessions"] = sessions
            report["harnesses"] = harnesses
        }
        #expect(s.rows.first { $0.kind == .session }?.title == "cod\\u{202E}fdp.exe")
    }
}

@MainActor
@Suite("Coding agents model")
struct AgentsModelTests {
    private let fake: FakeCLI

    init() throws {
        fake = try FakeCLI()
        try fake.put("agents-fast.json", Fixture.data("agents-fast"))
        try fake.put("agents.json", Fixture.data("agents-report"))
    }

    private func model(runner: Bool = true) -> AgentsModel {
        AgentsModel(runner: runner ? CLIRunner(executable: fake.executable) : nil, home: "/Users/example")
    }

    /// Polls until `condition` holds, for about ten seconds.
    private func until(_ condition: () -> Bool) async throws {
        for _ in 0..<1_000 where !condition() {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(condition())
    }

    private static let fastArgs = "disk agents --json --no-sizes"
    private static let sizesArgs = "disk agents --json"

    @Test("the sessions show after the first run, the sizes after the second")
    func twoRuns() async throws {
        try fake.open("agents.gated")
        let m = model()
        let refresh = Task { await m.refresh() }
        try await until { m.measuring }
        #expect(m.report?.sizes == false)
        #expect(m.summary?.rows.contains { $0.kind == .session } == true)
        try fake.open("agents-go")
        await refresh.value
        #expect(m.report?.sizes == true)
        #expect(!m.refreshing && !m.measuring)
        #expect(m.message == nil)
        #expect(try fake.runs().filter { $0.event == "start" }.map(\.args) == [Self.fastArgs, Self.sizesArgs])
    }

    @Test("a failed sizes run keeps the first report's sessions, without sizes")
    func failedSizes() async throws {
        try fake.put("agents.exit", Data("1".utf8))
        let m = model()
        await m.refresh()
        #expect(m.report?.sizes == false)
        #expect(m.summary?.rows.contains { $0.kind == .session } == true)
        #expect(m.message == "The sizes could not be measured: Listing the coding agents failed (exit 1).")
    }

    @Test("a failed first run shows its message and no rows")
    func failedFast() async throws {
        try fake.put("agents-fast.exit", Data("1".utf8))
        let m = model()
        await m.refresh()
        #expect(m.report == nil)
        #expect(m.message == "Listing the coding agents failed (exit 1).")
        #expect(try fake.runs().filter { $0.event == "start" }.count == 1, "no sizes run after a failed first run")
    }

    @Test("a Refresh while the two runs go starts no third run")
    func joins() async throws {
        try fake.open("agents-fast.gated")
        let m = model()
        let first = Task { await m.refresh() }
        try await fake.waitForStart(Self.fastArgs)
        let second = Task { await m.refresh() }
        try await Task.sleep(for: .milliseconds(50))
        try fake.open("agents-fast-go")
        await first.value
        await second.value
        #expect(try fake.runs().filter { $0.event == "start" }.map(\.args) == [Self.fastArgs, Self.sizesArgs])

        // A later refresh runs both again: liveness is never kept.
        await m.refresh()
        #expect(try fake.runs().filter { $0.event == "start" }.count == 4)
    }

    @Test("without the embedded CLI, a message and no run")
    func noRunner() async {
        let m = model(runner: false)
        #expect(!m.canRefresh)
        await m.refresh()
        #expect(m.report == nil)
        #expect(m.message?.contains("embedded CLI is missing") == true)
    }
}
