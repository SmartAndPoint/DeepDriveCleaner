import Foundation
import Testing
@testable import DDCKit

@Suite("Report summary")
struct ReportSummaryTests {
    @Test("with the scan-time settings the summary reproduces the report's own totals")
    func reproducesTheReport() throws {
        let report = try Fixture.groupsReport()
        let summary = ReportSummary.make(report: report, settings: try Fixture.groupsSettings(), rules: try Fixture.rules())

        #expect(summary.classes.map(\.riskClass) == report.totals.classes.map(\.class))
        for (row, total) in zip(summary.classes, report.totals.classes) {
            #expect(UInt64(row.count) == total.count)
            #expect(row.bytes == total.bytes)
            #expect(UInt64(row.selectedCount) == total.selectedCount)
            #expect(row.selectedBytes == total.selectedBytes)
        }
        #expect(summary.reclaimable == report.totals.selectedReclaimableBytes)
        #expect(summary.selectedBytes(including: [.safe, .rebuildable, .review, .userData]) == report.totals.selectedBytes)
        #expect(UInt64(summary.ignoredCount) == report.totals.ignoredCount)
        #expect(summary.ignoredBytes == report.totals.ignoredBytes)
        #expect(summary.threshold == 1_048_576)

        let unread = ReportSummary.make(report: report, settings: nil, rules: try Fixture.rules())
        #expect(unread.classes == summary.classes, "no settings file: the report's own flags give the same")
    }

    @Test("ignored findings are left out of the classes, the selection and the groups")
    func ignoredAreLeftOut() throws {
        let report = try Fixture.groupsReport()
        let summary = ReportSummary.make(report: report, settings: try Fixture.groupsSettings(), rules: try Fixture.rules())
        let ignored = report.findings.filter(\.ignored)
        #expect(Set(ignored.map(\.rule)) == ["cache.pip", "cache.npm", "user.pictures"])
        #expect(summary.foundBytes == report.findings.filter { !$0.ignored }.reduce(0) { $0 + $1.bytes })
        #expect(summary.row(.userData)?.count == 1, "the pictures under min_size are left out")
    }

    @Test("a type switched off after the scan leaves the summary at once")
    func ruleSwitchedOffLater() throws {
        let report = try Fixture.groupsReport()
        var settings = try Fixture.groupsSettings()
        let before = ReportSummary.make(report: report, settings: settings, rules: try Fixture.rules())
        settings.disabledRules.append("app.chrome-cache")
        let after = ReportSummary.make(report: report, settings: settings, rules: try Fixture.rules())

        let chrome = try #require(report.findings.first { $0.rule == "app.chrome-cache" })
        #expect(after.row(.safe)?.selectedBytes == (before.row(.safe)?.selectedBytes ?? 0) - chrome.bytes)
        #expect(after.reclaimable == before.reclaimable - chrome.reclaimableBytes)
        #expect(after.ignoredCount == before.ignoredCount + 1)
        #expect(!after.topGroups.contains { $0.name == "App caches" })
    }

    @Test("a minimum size raised after the scan keeps a finding of exactly that size and drops one a byte under")
    func minSizeBoundary() throws {
        let report = try Fixture.groupsReport()
        let target = try #require(report.findings.first { $0.rule == "rust.target" })
        var settings = try Fixture.groupsSettings()

        settings.minSize = target.bytes
        let equal = ReportSummary.make(report: report, settings: settings, rules: try Fixture.rules())
        #expect(equal.topGroups.contains { $0.name == "Build outputs" }, "bytes == min_size is kept")

        settings.minSize = target.bytes + 1
        let over = ReportSummary.make(report: report, settings: settings, rules: try Fixture.rules())
        #expect(!over.topGroups.contains { $0.name == "Build outputs" }, "a byte under min_size is ignored")
        #expect(over.row(.rebuildable)?.selectedBytes == (equal.row(.rebuildable)?.selectedBytes ?? 0) - target.bytes)
    }

    @Test("ddc's own leftovers are never left out")
    func tombstoneIsNeverIgnored() throws {
        let data = try Fixture.editedFindings("scan-report-groups") { findings in
            findings[0]["rule"] = ReportSummary.tombstone
        }
        let report = try ScanReport.read(data)
        var settings = try Fixture.groupsSettings()
        settings.disabledRules.append(ReportSummary.tombstone)
        settings.minSize = UInt64.max
        let summary = ReportSummary.make(report: report, settings: settings, rules: try Fixture.rules())
        #expect(summary.row(.safe)?.count == 1, "everything else is under min_size; the tombstone stays")
        #expect(summary.topGroups.map(\.name) == ["Interrupted cleanups"])
    }

    @Test("the top four groups rank by default-selected size and say how many projects are idle")
    func topGroups() throws {
        let summary = ReportSummary.make(
            report: try Fixture.groupsReport(),
            settings: try Fixture.groupsSettings(),
            rules: try Fixture.rules()
        )
        #expect(summary.topGroups.map(\.name) == ["Package caches", "node_modules", "Python environments", "Build outputs"])
        let modules = try #require(summary.topGroups.first { $0.name == "node_modules" })
        #expect((modules.selectedCount, modules.count, modules.projects) == (1, 2, true), "1 idle project of 2")
        let caches = try #require(summary.topGroups.first)
        #expect((caches.selectedCount, caches.projects) == (1, false), "the tiny npx cache is ignored")
        #expect(!summary.topGroups.contains { $0.name == "ML models" }, "nothing selected there")
    }

    @Test("without the rules list there are no groups yet")
    func noRulesNoGroups() throws {
        let summary = ReportSummary.make(report: try Fixture.groupsReport(), settings: try Fixture.groupsSettings(), rules: nil)
        #expect(summary.topGroups.isEmpty)
        #expect(summary.reclaimable > 0)
    }

    @Test("a report of the 0.1.0 CLI, without the optional fields, still summarises")
    func oldReport() throws {
        let report = try ScanReport.read(try Fixture.data("scan-report"))
        #expect(report.volumes.isEmpty && report.totals.ignoredCount == 0 && !report.findings.contains(where: \.ignored))
        let summary = ReportSummary.make(report: report, settings: SharedSettings(), rules: try Fixture.rules())
        #expect(summary.reclaimable == report.totals.selectedReclaimableBytes)
        #expect(summary.threshold == SharedSettings.defaultThreshold)
    }

    @Test("selected to clean follows the class switches; the reclaimable amount does not")
    func switches() throws {
        let summary = ReportSummary.make(
            report: try Fixture.groupsReport(),
            settings: try Fixture.groupsSettings(),
            rules: try Fixture.rules()
        )
        let safe = try #require(summary.row(.safe)).selectedBytes
        let rebuildable = try #require(summary.row(.rebuildable)).selectedBytes
        #expect(summary.selectedBytes(including: [.safe, .rebuildable]) == safe + rebuildable)
        #expect(summary.selectedBytes(including: [.rebuildable]) == rebuildable)
        #expect(summary.selectedBytes(including: []) == 0)
    }
}
