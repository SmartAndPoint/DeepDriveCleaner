import Foundation
import Testing
@testable import DDCKit

/// `last-disk.json` as the app reads it (story "Whole disk in the app"). The
/// fixtures are written by hand; `crates/ddc-core/tests/disk.rs` checks that
/// the CLI could have written them.
@Suite("Whole-disk report")
struct DiskReportTests {
    @Test("a finished report: container, home from the last scan, nested and partial areas, runtimes")
    func finished() throws {
        let r = try DiskReport.read(Fixture.data("disk-report"))
        #expect(!r.cancelled)
        #expect(r.container?.source == .diskutil)
        #expect(r.home.source == .lastScan && r.home.asOf == "2026-10-19T08:04:55Z" && !r.home.partial)
        #expect(r.areas.map(\.id) == ["applications", "library", "opt", "homebrew", "private", "temporary-folders", "simulator-runtimes"])
        let homebrew = try #require(r.areas.first { $0.id == "homebrew" })
        #expect(homebrew.parent == "opt" && homebrew.action == .brewCleanup && !homebrew.partial)
        let temp = try #require(r.areas.first { $0.id == "temporary-folders" })
        #expect(temp.partial && temp.entriesRead == 1_000_000 && temp.unreadable == 185)
        #expect(r.simulators?.status == .listed && r.simulators?.runtimes.count == 1)
        #expect(r.notCounted.bytes == 56 << 30 && !r.notCounted.inconsistent && r.notCounted.causes.isEmpty)
        #expect(r.notCounted.holds == [DiskReport.Held(kind: .cloud, path: "/Users/example/Library/CloudStorage")])
        #expect(r.snapshots.count == 0)
    }

    @Test("a stopped report: home measured now, every area partial, no runtimes, rows that overlap")
    func cancelled() throws {
        let r = try DiskReport.read(Fixture.data("disk-cancelled"))
        #expect(r.cancelled)
        #expect(r.container == nil && r.simulators == nil && r.snapshots.count == nil)
        #expect(r.home.source == .walk && r.home.partial && r.home.reclaimableBytes == nil)
        #expect(!r.areas.isEmpty && r.areas.allSatisfy(\.partial))
        #expect(r.notCounted.inconsistent && r.notCounted.causes == [.clonesCountedTwice])
        #expect(try #require(r.notCounted.bytes) < 0)
    }

    @Test("a report of another schema version is not decoded")
    func otherSchema() throws {
        let data = try Fixture.edited("disk-report") { $0["schema_version"] = 2 }
        #expect(throws: ScanReport.ReadError.otherSchema(2)) { try DiskReport.read(data) }
    }

    @Test("an unknown enum value is an error, never a guess")
    func strictEnums() throws {
        let data = try Fixture.edited("disk-report") { json in
            var home = json["home"] as? [String: Any] ?? [:]
            home["source"] = "estimate"
            json["home"] = home
        }
        #expect(throws: DecodingError.self) { try DiskReport.read(data) }
    }
}
