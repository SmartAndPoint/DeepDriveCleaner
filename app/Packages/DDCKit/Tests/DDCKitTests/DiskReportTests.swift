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
        #expect(r.areas.map(\.id) == ["applications", "library", "opt", "homebrew", "conda", "private", "temporary-folders", "simulator-runtimes"])
        let homebrew = try #require(r.areas.first { $0.id == "homebrew" })
        #expect(homebrew.parent == "opt" && homebrew.action == .brewCleanup && !homebrew.partial)
        let temp = try #require(r.areas.first { $0.id == "temporary-folders" })
        #expect(temp.partial && temp.entriesRead == 1_000_000 && temp.unreadable == 185)
        #expect(r.simulators?.status == .listed && r.simulators?.runtimes.count == 1)
        #expect(r.notCounted.bytes == 56 << 30 && !r.notCounted.inconsistent && r.notCounted.causes.isEmpty)
        #expect(r.notCounted.holds == [DiskReport.Held(kind: .cloud, path: "/Users/example/Library/CloudStorage")])
        #expect(r.snapshots.count == 0)
    }

    @Test("the /opt conda installs: each one's package cache and environments, with what removing each alone frees")
    func conda() throws {
        let r = try DiskReport.read(Fixture.data("disk-report"))
        #expect(r.areas.first { $0.id == "conda" }?.action == .condaClean)
        let installs = try #require(r.conda?.installs)
        #expect(installs.map(\.prefix) == ["/opt/miniconda3", "/opt/anaconda3"])
        #expect(installs[0].packages?.path == "/opt/miniconda3/pkgs" && installs[0].packages?.writable == true)
        #expect(installs[0].environments.map(\.path) == ["/opt/miniconda3/envs/ml", "/opt/miniconda3/envs/old"])
        #expect(installs[0].environments[0].bytes == 2 << 30 && installs[0].environments[0].sharedBytes == 1 << 30)
        #expect(installs[1].packages?.writable == false && installs[1].environments.allSatisfy { !$0.writable })
        let without = try DiskReport.read(Fixture.edited("disk-report") { $0["conda"] = nil })
        #expect(without.conda == nil)
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
        for version in [1, 3] {
            let data = try Fixture.edited("disk-report") { $0["schema_version"] = version }
            #expect(throws: ScanReport.ReadError.otherSchema(version)) { try DiskReport.read(data) }
        }
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
