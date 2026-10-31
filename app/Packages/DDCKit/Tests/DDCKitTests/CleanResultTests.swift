import Foundation
import Testing
@testable import DDCKit

@Suite("Review window: the result")
struct CleanResultTests {
    /// A report with every outcome, in the clean schema's shape.
    private func report(_ items: [String], version: Int = 6) throws -> CleanReport {
        let json = """
            {"schema_version": \(version), "ddc_version": "0.2.0", "platform": "macos", "capabilities": ["private-size", "in-use-check", "dataless-files", "full-disk-access", "local-snapshots", "whole-disk", "coding-agents", "homebrew", "schedule", "notifications"], "cancelled": true, "failed": 2, "local_snapshots": 1,
             "journal": "/Users/example/Library/Logs/DeepDriveCleaner/actions.jsonl",
             "volumes": [{"mount_point": "/System/Volumes/Data", "free_before": 1000, "free_after": 4000},
                         {"mount_point": "/Volumes/Work", "free_before": 10, "free_after": 10}],
             "items": [\(items.joined(separator: ","))]}
            """
        return try CleanReport.read(Data(json.utf8))
    }

    private func item(_ path: String, _ outcome: String, bytes: Int? = nil, ino: Int = 1) -> String {
        let size = bytes.map { #", "bytes": \#($0)"# } ?? ""
        return #"{"path": "/Users/example/\#(path)", "dev": 16777232, "ino": \#(ino), "rule": "cache.npm", "class": "safe", "action": "delete"\#(size), \#(outcome)}"#
    }

    @Test("skips group by cause in plain language, biggest first; running apps merge, processes stay apart")
    func causes() throws {
        let items = try ReviewFixture.items()
        let confirmation = DDCKit.Confirmation(items, selection: try ReviewFixture.recordedSelection(items), deletePermanently: false)
        let p1 = try #require(items.rows.first { $0.finding.path.hasSuffix("p1/node_modules") }).finding
        let report = try report([
            item("a", #""outcome": "skipped", "reason": "owner-running", "pid": 1, "name": "Spotify""#, bytes: 100),
            item("b", #""outcome": "skipped", "reason": "owner-running", "pid": 2, "name": "Google Chrome""#, bytes: 200),
            item("c", #""outcome": "skipped", "reason": "owner-running", "pid": 1, "name": "Spotify""#, bytes: 50),
            item("d", #""outcome": "skipped", "reason": "in-use", "pid": 3, "name": "node""#, bytes: 10),
            item("e", #""outcome": "skipped", "reason": "in-use", "pid": 4, "name": "node""#, bytes: 20),
            // Refused before the CLI measured it: its size comes from the sheet.
            #"{"path": "\#(p1.path)", "dev": \#(p1.dev), "ino": \#(p1.ino), "rule": "node.node_modules", "class": "rebuildable", "action": "delete", "outcome": "skipped", "reason": "changed-since-scan"}"#,
            item("g", #""outcome": "native", "commands": [{"program": "go", "args": ["clean", "-modcache"], "success": false, "code": 1}]"#, bytes: 7),
            item("h", #""outcome": "failed", "error": "Permission denied""#, bytes: 3),
            item("i", #""outcome": "not-run""#, bytes: 5),
            item("j", #""outcome": "deleted""#, bytes: 1000),
            item("k", #""outcome": "trashed""#, bytes: 500),
        ])
        let volumes = try Fixture.groupsReport().volumes
        let result = CleanResult.make(report: report, confirmation: confirmation, volumes: volumes)

        #expect(result.causes.map(\.title) == ["Changed since the scan", "Spotify and Google Chrome were running", "In use by node"])
        #expect(result.causes.map(\.count) == [1, 3, 2])
        #expect(result.causes.map(\.bytes) == [p1.bytes, 350, 30])
        #expect(result.causes[2].paths == ["~/d", "~/e"])
        #expect(result.skipped == CleanResult.Tally(count: 6, bytes: 380 + p1.bytes))
        #expect(result.deleted == CleanResult.Tally(count: 1, bytes: 1000))
        #expect(result.trashed == CleanResult.Tally(count: 1, bytes: 500))
        #expect(result.toolsRun == CleanResult.Tally(count: 1, bytes: 7))
        #expect(result.notRun == CleanResult.Tally(count: 1, bytes: 5))
        #expect(result.failed == 2)
        #expect(result.failures == [
            CleanResult.Failure(path: "~/g", error: "go clean -modcache failed (exit 1)"),
            CleanResult.Failure(path: "~/h", error: "Permission denied"),
        ])
        #expect(result.cancelled && result.localSnapshots == 1)
        #expect(result.volumes.map(\.name) == ["The startup disk", "Work"])
        #expect(result.volumes[0].total == volumes.first?.totalBytes)
        #expect(result.volumes[1].total == nil)
    }

    @Test("Docker Desktop not answering is a skip that says why; a prune that failed says what docker said")
    func dockerNotReady() throws {
        let items = try ReviewFixture.items()
        let confirmation = DDCKit.Confirmation(items, selection: try ReviewFixture.recordedSelection(items), deletePermanently: false)
        let docker = #""rule": "vm.docker-desktop", "class": "review", "action": "native-only""#
        let entries = [
            #"{"path": "/Users/example/a", "dev": 1, "ino": 1, \#(docker), "bytes": 100, "outcome": "skipped", "reason": "tool-not-ready", "program": "docker", "detail": "Docker Desktop is not running or not answering"}"#,
            #"{"path": "/Users/example/b", "dev": 1, "ino": 2, \#(docker), "bytes": 7, "outcome": "native", "commands": [{"program": "docker", "args": ["--context", "desktop-linux", "builder", "prune", "-f"], "success": true, "code": 0, "output": "Total reclaimed space: 0B"}, {"program": "docker", "args": ["--context", "desktop-linux", "image", "prune", "-f"], "success": false, "code": 1, "output": "Client: Docker Engine\nError response from daemon: a prune operation is already running\n"}]}"#,
        ]
        let report = try report(entries)

        let notReady = SkipReason.toolNotReady(program: "docker", detail: "Docker Desktop is not running or not answering")
        #expect(report.items[0].outcome == .skipped(notReady))
        guard case .native(let commands) = report.items[1].outcome else { throw CancellationError() }
        #expect(commands[0].output == "Total reclaimed space: 0B")
        #expect(SkipReason.tags.contains("tool-not-ready"))
        #expect(CleanWording.reason(notReady) == "Docker Desktop is not running or not answering")
        #expect(CleanWording.reason(.nativeUnavailable(program: "docker")) == "docker is not installed")

        let result = CleanResult.make(report: report, confirmation: confirmation, volumes: [])
        #expect(result.causes.map(\.title) == ["Docker Desktop is not running or not answering"])
        #expect(result.failures == [
            CleanResult.Failure(path: "~/b", error: "docker image prune failed: Error response from daemon: a prune operation is already running"),
        ])

        // An older CLI's report, without the reason, is refused whole.
        #expect(throws: CleanReport.ReadError.otherSchema(3)) { try self.report(entries, version: 3) }
    }

    @Test("more than three running apps are named three and counted")
    func manyApps() {
        let apps = ["Spotify", "Chrome", "Brave", "Slack", "Zoom"]
        #expect(CleanWording.title(.appsRunning, apps: apps) == "Spotify, Chrome, Brave and 2 more were running")
        #expect(CleanWording.title(.appsRunning, apps: ["Spotify"]) == "Spotify was running")
    }

    @Test("a clean that moved items to the Trash says how much went there; one that only deleted does not, and keeps the APFS note")
    func trashed() throws {
        let items = try ReviewFixture.items()
        let confirmation = DDCKit.Confirmation(items, selection: try ReviewFixture.recordedSelection(items), deletePermanently: false)
        let volumes = try Fixture.groupsReport().volumes
        let moved = CleanResult.make(
            report: try report([item("a", #""outcome": "trashed""#, bytes: 3 << 20), item("b", #""outcome": "trashed""#, bytes: 1 << 20, ino: 2)]),
            confirmation: confirmation, volumes: volumes
        )
        #expect(CleanWording.trashed(moved) == "4.0 MiB went to the Trash. It comes back when you empty the Trash.")
        #expect(CleanWording.apfsNote(moved) == nil, "a move to the Trash frees nothing yet")

        let deleted = CleanResult.make(report: try report([item("a", #""outcome": "deleted""#, bytes: 1000)]), confirmation: confirmation, volumes: volumes)
        #expect(CleanWording.trashed(deleted) == nil)
        #expect(CleanWording.apfsNote(deleted) == "APFS may take a moment to give the space back.")
        let tool = CleanResult.make(
            report: try report([item("g", #""outcome": "native", "commands": [{"program": "go", "args": ["clean", "-cache"], "success": true, "code": 0}]"#, bytes: 7)]),
            confirmation: confirmation, volumes: volumes
        )
        #expect(CleanWording.apfsNote(tool) != nil, "a tool frees space now too")
    }
}
