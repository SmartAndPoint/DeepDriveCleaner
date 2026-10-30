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
