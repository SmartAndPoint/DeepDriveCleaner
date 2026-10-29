import Foundation
import Testing
@testable import DDCKit

/// Story "Whole disk: Show in Finder and the honest action for each row":
/// the button each Whole disk row offers where DeepDriveCleaner does not act
/// itself. Fixtures only; nothing is opened.
@Suite("Show in Finder and Open Software Update")
struct RevealTests {
    /// The disk fixture with `edit` applied to the area `id`.
    private func report(editing id: String, _ edit: @escaping (inout [String: Any]) -> Void) throws -> DiskReport {
        try DiskReport.read(Fixture.edited("disk-report") { json in
            var areas = json["areas"] as? [[String: Any]] ?? []
            if let i = areas.firstIndex(where: { $0["id"] as? String == id }) { edit(&areas[i]) }
            json["areas"] = areas
        })
    }

    private func rows(_ report: DiskReport) -> [String: DiskSummary.Row] {
        let summary = DiskSummary.make(report, scan: nil, summary: nil, included: [.safe], now: Date())
        return Dictionary(summary.rows.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
    }

    @Test("a finder row shows Show in Finder with its folders; a row without one, and the home and not-counted rows, show nothing")
    func rowsFromTheReport() throws {
        let rows = rows(try DiskReport.read(Fixture.data("disk-report")))
        #expect(rows["applications"]?.reveal == .finder(["/Applications"]))
        #expect(rows["applications"]?.reveal?.title == "Show in Finder")
        #expect(rows["homebrew"]?.reveal == .finder(["/opt/homebrew"]), "beside its tool's offer")
        #expect(rows["homebrew"]?.offer != nil)
        #expect(rows["private"] != nil && rows["private"]?.reveal == nil)
        #expect(rows["temporary-folders"]?.reveal == nil)
        #expect(rows["home"]?.reveal == nil)
        #expect(rows["not-counted"]?.reveal == nil)
    }

    @Test("software-update opens the Software Update pane")
    func softwareUpdate() throws {
        let report = try report(editing: "library") { $0["reveal"] = "software-update" }
        let reveal = try #require(rows(report)["library"]?.reveal)
        #expect(reveal == .softwareUpdate)
        #expect(reveal.title == "Open Software Update")
        #expect(AreaActions.softwareUpdateURL.absoluteString == "x-apple.systempreferences:com.apple.Software-Update-Settings.extension")
    }

    @Test("an unknown value, or none, is no button, and the report still reads")
    func lenient() throws {
        let unknown = try report(editing: "applications") { $0["reveal"] = "terminal" }
        #expect(unknown.areas.first { $0.id == "applications" }?.reveal == nil)
        #expect(rows(unknown)["applications"]?.reveal == nil)
        let missing = try report(editing: "applications") { $0.removeValue(forKey: "reveal") }
        #expect(rows(missing)["applications"]?.reveal == nil)
    }

    @Test("Finder gets the firmlinked path, never the Data volume's")
    func userPath() throws {
        #expect(AreaActions.userPath("/System/Volumes/Data/opt/miniconda3") == "/opt/miniconda3")
        #expect(AreaActions.userPath("/System/Volumes/Data") == "/")
        #expect(AreaActions.userPath("/Applications") == "/Applications")
        #expect(AreaActions.userPath("/System/Volumes/DataX/a") == "/System/Volumes/DataX/a")

        let report = try report(editing: "opt") { $0["paths"] = ["/System/Volumes/Data/opt"] }
        #expect(rows(report)["opt"]?.reveal == .finder(["/opt"]))
    }

    @Test("Show in Finder selects only the folders that exist now, in order")
    func existing() {
        let paths = ["/opt/miniconda3", "/opt/anaconda3", "/opt/miniforge3"]
        let present: Set = ["/opt/miniconda3", "/opt/miniforge3"]
        #expect(AreaActions.existing(paths, exists: present.contains) == ["/opt/miniconda3", "/opt/miniforge3"])
        #expect(AreaActions.existing(paths, exists: { _ in false }).isEmpty)
    }
}
