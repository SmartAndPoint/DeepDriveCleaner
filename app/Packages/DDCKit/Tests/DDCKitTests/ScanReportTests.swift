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
