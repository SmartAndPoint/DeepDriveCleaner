import Foundation
import Testing
@testable import DDCKit

@Suite("ScanReport")
struct ScanReportTests {
    static func fixture(_ name: String) throws -> URL {
        try #require(Bundle.module.url(forResource: name, withExtension: "json", subdirectory: "Fixtures"))
    }

    @Test("the runner decodes a recorded scan report")
    func decodesRecordedReport() async throws {
        let fake = try FakeCLI()
        let result = await CLIRunner(executable: fake.executable)
            .run(.scan, ["report", try Self.fixture("scan-report").path])

        let report = try ScanReport.read(result.stdout)
        #expect(report.schemaVersion == ScanReport.schemaVersion)
        #expect(report.findings.map(\.rule) == ["cache.npm", "node.node_modules", "user.pictures"])
        #expect(report.findings.map(\.class) == [.safe, .rebuildable, .userData])
        #expect(report.totals.classes.map(\.class) == [.safe, .rebuildable, .review, .userData])
        #expect(report.totals.selectedBytes == report.findings.filter(\.selected).reduce(0) { $0 + $1.bytes })
    }

    /// Story "Protected by you: say what it is, and Remove Protection": the
    /// kept findings' `protection` is a new optional field of schema 2, so
    /// this app, which does not read it yet, still reads the report.
    @Test("a report whose kept findings say what protects them reads as before")
    func decodesReportWithProtection() throws {
        let report = try ScanReport.read(try Data(contentsOf: try Self.fixture("scan-report-protection")))
        #expect(report.schemaVersion == ScanReport.schemaVersion)
        #expect(report.findings.map(\.rule) == ["user.kept", "user.kept", "user.kept"])
        #expect(report.findings.map(\.notes) == [[.keptByMarker], [.excluded], [.excluded]])
        #expect(report.findings.allSatisfy { $0.class == .userData && !$0.selected })
    }

    /// ADR "Agent footprint: worktrees, harness data and bounded consent for
    /// agents", §1: a coding agent's finding carries `origin`, a new optional
    /// field of schema 2, so this app, which does not read it yet, still
    /// reads the report.
    @Test("a report whose findings say which coding agent they come from reads as before")
    func decodesReportWithOrigin() throws {
        let report = try ScanReport.read(try Data(contentsOf: try Self.fixture("scan-report-origin")))
        #expect(report.schemaVersion == ScanReport.schemaVersion)
        #expect(report.findings.map(\.rule) == ["agent.claude-code-scratch", "agent.claude-code-transcripts"])
        #expect(report.findings.map(\.class) == [.safe, .review])
        #expect(report.findings.map(\.selected) == [true, false])
        #expect(report.findings.map(\.modified) == ["2026-10-29T09:12:44Z", "2026-10-30T17:40:02Z"])
        #expect(report.totals.selectedBytes == 8192)
    }

    /// Story "Harness data rules" (epic DDC-003): a session folder lists its
    /// sessions as entries, a live one locked `live-session`; an unknown
    /// `by` or `cause` does not decode.
    @Test("a session folder's entries decode with their liveness locks")
    func decodesSessionEntries() throws {
        let report = try ScanReport.read(try Data(contentsOf: try Self.fixture("scan-report-origin")))
        let contents = try #require(report.findings.last?.contents)
        #expect(contents.entries.map(\.bytes) == [36864, 8192])
        #expect(contents.entries.map(\.lock) == [.liveSession(session: "0a1b2c3d-0000-4000-8000-00000000000a", by: .record), nil])
        #expect((contents.restCount, contents.restBytes) == (1, 4096))
        let unknown = try JSONDecoder().decode(EntryLock.self, from: Data(#"{"reason":"liveness-unknown","cause":"records-skipped"}"#.utf8))
        #expect(unknown == .livenessUnknown(cause: .recordsSkipped))
        #expect(throws: (any Error).self) {
            try JSONDecoder().decode(EntryLock.self, from: Data(#"{"reason":"live-session","session":"s","by":"guess"}"#.utf8))
        }
        #expect(throws: (any Error).self) {
            try JSONDecoder().decode(EntryLock.self, from: Data(#"{"reason":"liveness-unknown","cause":"new-cause"}"#.utf8))
        }
    }

    /// A session folder's `contents` must not turn the Coding agents group into
    /// the user-data entries layout: the history stays a row beside the session
    /// folder, which is a subgroup of its sessions (story "Review window: pick a
    /// coding agent's sessions one by one").
    @Test("the Coding agents group keeps its other rows when a session folder lists entries")
    func codingAgentsGroupKeepsItsRows() throws {
        let data = try Fixture.editedFindings("scan-report-origin") { findings in
            var history = findings[findings.count - 1]
            history["path"] = "/Users/example/.claude/paste-cache"
            history["rule"] = "agent.claude-code-history"
            history["removal"] = "trash"
            history["ino"] = 999
            history.removeValue(forKey: "contents")
            findings.append(history)
        }
        let items = ReviewItems.make(report: try ScanReport.read(data), settings: nil, rules: try Fixture.rules())
        let layout = items.layout(ReviewItems.Entry(riskClass: .review, group: "Coding agents"), matching: "")
        #expect(layout.shape != .entries)
        // The history, and the two sessions of the transcripts folder.
        #expect(layout.count == 3)
        #expect(layout.blocks.count == 2)
        #expect(layout.selectAll == [try #require(items.rows.first { $0.finding.rule == "agent.claude-code-history" }).id])
    }

    @Test("a report of another schema version is unreadable")
    func otherSchemaIsUnreadable() throws {
        let data = try Data(contentsOf: try Self.fixture("scan-report-v1"))
        #expect(throws: ScanReport.ReadError.otherSchema(1)) { try ScanReport.read(data) }
    }

    @Test("the app reads the schema version the embedded CLI writes")
    func schemaVersionMatchesTheCLI() throws {
        // app/Packages/DDCKit/Tests/DDCKitTests/<file> -> the repository root
        var root = URL(filePath: #filePath)
        for _ in 0..<6 { root.deleteLastPathComponent() }
        let model = try String(contentsOf: root.appending(path: "crates/ddc-core/src/model.rs"), encoding: .utf8)
        let match = try #require(model.firstMatch(of: /pub const SCHEMA_VERSION: u32 = (\d+);/))
        #expect(Int(match.1) == ScanReport.schemaVersion)
    }
}
