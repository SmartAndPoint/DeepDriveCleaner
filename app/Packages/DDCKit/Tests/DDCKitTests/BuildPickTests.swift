import Foundation
import Testing
@testable import DDCKit

/// Story "Coding agents on Linux: ddc disk agents" (epic DDC-010): a coding
/// agent's builds folder (origin kind `builds`) lists its old builds, picked
/// one by one and moved to the Trash, never as sessions (safety ADR,
/// amendment of 2026-11-02). Fixture paths under `/Users/example` only.
@Suite("Coding agents: pick old builds one by one")
@MainActor
struct BuildPickTests {
    let data: TempDataDirectory
    let defaults: TempDefaults

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
    }

    static let versions = "/Users/example/.local/share/claude/versions"
    static let old = "\(versions)/2.1.100"
    static let newest = "\(versions)/2.1.130"

    /// The Coding agents report of `AgentsReviewTests`, with Claude Code's
    /// builds folder: one old build, free, and the newest, locked.
    static func report() throws -> Data {
        try AgentsReviewTests.report { findings in
            guard var builds = findings.first(where: { $0["rule"] as? String == "agent.claude-code-transcripts" }) else { return }
            builds["path"] = versions
            builds["rule"] = "agent.claude-code-builds"
            builds["hint"] = "An old Claude Code build. claude install <version> fetches it again."
            builds["ino"] = 339_569_900
            builds["origin"] = ["harness": "claude-code", "kind": "builds"]
            builds["contents"] = [
                "entries": [
                    ["path": old, "kind": "file", "bytes": 4096, "dev": 16_777_232, "ino": 339_569_901, "modified": "2026-10-01T08:00:00Z"],
                    [
                        "path": newest, "kind": "file", "bytes": 8192, "dev": 16_777_232, "ino": 339_569_902,
                        "modified": "2026-10-30T08:00:00Z", "lock": ["reason": "build-in-use", "by": "newest"],
                    ],
                ],
                "rest_count": 0,
                "rest_bytes": 0,
            ]
            findings.append(builds)
        }
    }

    private func items() throws -> ReviewItems {
        ReviewItems.make(report: try ScanReport.read(try Self.report()), settings: nil, rules: try Fixture.rules())
    }

    private static let entry = ReviewItems.Entry(riskClass: .review, group: AgentsSummary.group)

    @Test("a builds folder is a subgroup of its old builds, shown by their whole names, and never protected")
    func layout() throws {
        let items = try items()
        let folder = try #require(items.rows.first { $0.finding.rule == "agent.claude-code-builds" && $0.userEntry == nil })
        #expect(folder.lock == .builds)
        #expect(folder.lock?.text == "Not cleaned whole · pick its old builds one by one")
        let layout = items.layout(Self.entry, matching: "")
        let subgroups = layout.blocks.compactMap { if case .subgroup(let s) = $0 { s } else { nil } }
        let builds = try #require(subgroups.first { $0.kind == .builds })
        #expect(builds.subtitle.contains("2 old builds"))
        #expect(builds.items.map(\.shown) == ["2.1.100", "2.1.130"] || builds.items.map(\.shown) == ["2.1.130", "2.1.100"])
        #expect(builds.foldHint(expanded: false) == "Shows its builds")
        #expect(builds.foldHint(expanded: true) == "Hides its builds")
        let rows = items.rows.filter { $0.userEntry?.isBuild == true }
        #expect(rows.count == 2)
        #expect(rows.allSatisfy { $0.userEntry?.isSession == false })
        #expect(rows.allSatisfy { !items.canProtect($0) })
        let locked = try #require(rows.first { $0.finding.path == Self.newest })
        #expect(locked.lock == .entry(.buildInUse(by: .newest)))
        #expect(!locked.selectable)
    }

    @Test("with nothing picked, the row says to pick builds")
    func pickedSummary() throws {
        let snapshot = ReviewSnapshot(report: try ScanReport.read(try Self.report()), rules: try Fixture.rules())
        let review = makeReview(runner: nil, data: data, defaults: defaults) { snapshot }
        #expect(review.open(ReviewRequest(group: AgentsSummary.group, classes: [], confirm: false)))
        let items = try #require(review.items)
        let scope = items.rows.filter { $0.userEntry?.isBuild == true }.map(\.id)
        #expect(review.pickedSummary(scope) == "Pick builds below to move them to the Trash")
    }
}
