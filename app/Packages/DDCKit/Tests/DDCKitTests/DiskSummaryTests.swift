import Foundation
import Testing
@testable import DDCKit

/// The bar and the rows of the Whole disk view (story "Whole disk in the app").
@Suite("Whole-disk summary")
struct DiskSummaryTests {
    private let utc: Calendar = {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "UTC")!
        return calendar
    }()
    private let locale = Locale(identifier: "en_GB")
    private var now: Date { Wording.date(rfc3339: "2026-10-19T12:00:00Z")! }

    /// The popover's default: Safe and Rebuildable switched on.
    private static let popoverDefault: Set<ScanReport.RiskClass> = [.safe, .rebuildable]

    private func summary(_ name: String, scan: ScanReport? = nil, included: Set<ScanReport.RiskClass> = popoverDefault) throws -> DiskSummary {
        try summary(DiskReport.read(Fixture.data(name)), scan: scan, included: included)
    }

    private func summary(_ report: DiskReport, scan: ScanReport? = nil, included: Set<ScanReport.RiskClass> = popoverDefault) throws -> DiskSummary {
        let popover = try scan.map { ReportSummary.make(report: $0, settings: try Fixture.groupsSettings(), rules: nil) }
        return DiskSummary.make(report, scan: scan, summary: popover, included: included, now: now, calendar: utc, locale: locale)
    }

    private func coloured(_ s: DiskSummary) -> UInt64 {
        s.segments.filter { if case .frees = $0.tone { true } else { false } }.reduce(0) { $0 + $1.bytes }
    }

    @Test("the bar is the container: what the home folder frees by class, its rest, the areas, not counted, other volumes, free")
    func bar() throws {
        let report = try DiskReport.read(Fixture.data("disk-report"))
        let scan = try Fixture.groupsReport()
        let s = try summary("disk-report", scan: scan)
        #expect(s.segments.map(\.tone) == [.frees(.safe), .frees(.rebuildable), .home, .outside, .notCounted, .otherVolumes, .free])
        #expect(s.barTotal == report.container?.capacityBytes)
        let coloured = coloured(s)
        let popover = ReportSummary.make(report: scan, settings: try Fixture.groupsSettings(), rules: nil)
        #expect(coloured == popover.reclaimable(including: Self.popoverDefault), "the popover's number")
        #expect(s.rows[0].note.hasSuffix("\(ByteSize.human(coloured)) can be freed in Review and clean"))
        let home = s.segments.filter { $0.tone == .home }.reduce(coloured) { $0 + $1.bytes }
        #expect(home == report.home.bytes)
        let outside = s.segments.first { $0.tone == .outside }?.bytes
        #expect(outside == report.areas.reduce(0) { $0 + $1.bytes })
        let free = try #require(report.container).capacityBytes - report.container!.inUseBytes
        #expect(s.segments.last == DiskSummary.Segment(tone: .free, bytes: free))
        #expect(s.usage == "\(ByteSize.human(report.container!.inUseBytes)) used of \(ByteSize.human(report.container!.capacityBytes))")
    }

    @Test("a class switched off in the popover is not coloured, and the home row's number follows", arguments: [
        Set<ScanReport.RiskClass>([.safe]), [.rebuildable], [],
    ])
    func splitFollowsThePopover(included: Set<ScanReport.RiskClass>) throws {
        let scan = try Fixture.groupsReport()
        let s = try summary("disk-report", scan: scan, included: included)
        let popover = ReportSummary.make(report: scan, settings: try Fixture.groupsSettings(), rules: nil)
        #expect(coloured(s) == popover.reclaimable(including: included))
        #expect(s.segments.compactMap { if case .frees(let c) = $0.tone { c } else { nil } }.allSatisfy(included.contains))
        if coloured(s) > 0 {
            #expect(s.rows[0].note.hasSuffix("\(ByteSize.human(coloured(s))) can be freed in Review and clean"))
        }
    }

    @Test("the simulator row says what simctl gave", arguments: [
        ("listed", nil, "Listed runtimes 0 B"),
        ("first-launch-pending", "Xcode has not finished its first launch.", "Xcode has not finished its first launch."),
        ("failed", "simctl failed: exit 1", "simctl failed: exit 1"),
    ] as [(String, String?, String)])
    func simulatorNotes(status: String, message: String?, expected: String) throws {
        let data = try Fixture.edited("disk-report") { json in
            var sims = json["simulators"] as! [String: Any]
            sims["status"] = status
            sims["message"] = message ?? NSNull()
            sims["runtimes"] = [Any]()
            sims["listed_bytes"] = 0
            json["simulators"] = sims
        }
        let row = try #require(try summary(DiskReport.read(data)).rows.first { $0.id == "simulator-runtimes" })
        #expect(row.note.contains(expected))
    }

    @Test("the home segment stays whole when the disk report's home row is another scan")
    func splitOnlyForTheSameScan() throws {
        let other = try ScanReport.read(Fixture.edited("scan-report-groups") { $0["generated_at"] = "2026-10-18T08:00:00Z" })
        let s = try summary("disk-report", scan: other)
        #expect(!s.segments.contains { if case .frees = $0.tone { true } else { false } })
        #expect(try summary("disk-report").segments.first?.tone == .home)
    }

    @Test("rows: the home row first with its source, nested areas under their parent, not counted last")
    func rows() throws {
        let s = try summary("disk-report")
        #expect(s.rows.map(\.id) == ["home", "applications", "library", "opt", "homebrew", "private", "temporary-folders", "simulator-runtimes", "runtime 9A1B2C3D-0000-4000-8000-000000000001", "not-counted"])
        #expect(s.rows[0].note.hasPrefix("From the scan today at 8:04 · 2.5 MiB can be freed"))
        #expect(s.rows.filter(\.nested).map(\.id) == ["homebrew", "temporary-folders", "runtime 9A1B2C3D-0000-4000-8000-000000000001"])
        let temp = try #require(s.rows.first { $0.id == "temporary-folders" })
        #expect(temp.size.hasPrefix("at least ") && temp.note.contains("1.0 M entries read"))
        let runtimes = try #require(s.rows.first { $0.id == "simulator-runtimes" })
        #expect(runtimes.note.contains("Listed runtimes") && runtimes.note.contains("more in their folders"))
        let nc = try #require(s.rows.last)
        #expect(nc.size == "56.0 GiB" && nc.note.contains("1 cloud folder not read") && nc.note.contains("339 folders could not be read"))
        #expect(!s.stopped)
    }

    @Test("a stopped report reads at least, and rows that overlap draw no not-counted segment")
    func stopped() throws {
        let s = try summary("disk-cancelled")
        #expect(s.stopped && s.usage == nil)
        #expect(s.rows.dropLast().allSatisfy { $0.size.hasPrefix("at least ") })
        #expect(s.rows[0].note.hasPrefix("Measured now"))
        #expect(!s.segments.contains { $0.tone == .notCounted || $0.tone == .free })
        let nc = try #require(s.rows.last)
        #expect(nc.size.hasPrefix("−") && nc.note.contains("APFS clones were counted twice"))
    }
}
