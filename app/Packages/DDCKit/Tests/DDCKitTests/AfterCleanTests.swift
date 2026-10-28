import Foundation
import Testing
@testable import DDCKit

/// Story "Review window: after a clean, only skips that can clear stay
/// selected": what the reload after a clean keeps selected, what the result
/// says, the tool notes on the rows, and the sheet while it slides away.
/// `FakeCLI` replays a hand-written clean report with one item of each kind
/// over the groups fixture, with a Docker and a Podman item added. The
/// mapping of every reason is `SkipSummaryTests.afterCleanTable`.
@Suite("Review window: after a clean", .timeLimit(.minutes(1)))
@MainActor
struct AfterCleanTests {
    /// The window's clock and its report, moved by the tests.
    final class World {
        var now = Date(timeIntervalSince1970: 2_000_000_000)
        var snapshot: ReviewSnapshot

        init(_ snapshot: ReviewSnapshot) { self.snapshot = snapshot }
    }

    let data: TempDataDirectory
    let defaults: TempDefaults
    let fake: FakeCLI
    let events = ReviewEvents()
    let world: World
    let review: ReviewModel
    let items: ReviewItems

    static let docker = "/Users/example/Library/Containers/com.docker.docker"
    static let podman = "/Users/example/.local/share/containers"

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
        let report = try Self.report(generatedAt: nil)
        try data.write(lastScan: report)
        fake = try FakeCLI(lastScan: data.directory.lastScan)
        try fake.put("dry-run.jsonl", Data(contentsOf: Fixture.url("clean-dry-run", extension: "jsonl")))
        world = World(try ReviewFixture.snapshot(report: report, settingsIn: data))
        let world = world
        review = makeReview(runner: CLIRunner(executable: fake.executable), data: data, defaults: defaults, events: events, clock: { world.now }) {
            world.snapshot
        }
        _ = review.open(ReviewRequest(group: nil, classes: [.safe, .rebuildable], confirm: false))
        items = try #require(review.items)
    }

    /// The groups report with a Docker and a Podman item, `generatedAt` when given.
    static func report(generatedAt: String?) throws -> Data {
        let vm = ["class": "review", "rule_class": "review", "removal": "native-only", "selected": false, "not_selected": ["reason": "review"]] as [String: Any]
        let docker = try ReviewFixture.finding(8, vm.merging(["path": docker, "rule": "vm.docker-desktop", "ino": 900_001]) { $1 })
        let podman = try ReviewFixture.finding(8, vm.merging(["path": podman, "rule": "vm.podman", "ino": 900_002]) { $1 })
        return try Fixture.edited("scan-report-groups") { json in
            json["findings"] = (json["findings"] as? [[String: Any]] ?? []) + [docker, podman]
            if let generatedAt { json["generated_at"] = generatedAt }
        }
    }

    private func id(_ suffix: String) throws -> Int { try ReviewFixture.id(items, suffix) }

    /// A report item for the row ending in `suffix`, with `outcome` (JSON members).
    private func item(_ suffix: String, action: String = "delete", _ outcome: String) throws -> String {
        let finding = try #require(items.rows.first { $0.finding.path.hasSuffix(suffix) }).finding
        return #"{"path": "\#(finding.path)", "dev": \#(finding.dev), "ino": \#(finding.ino), "rule": "\#(finding.rule)", "class": "\#(finding.class.rawValue)", "action": "\#(action)", "bytes": \#(finding.bytes), \#(outcome)}"#
    }

    /// The eight kinds of the story, one item each.
    private func eightKinds() throws -> Data {
        let items = [
            try item("Google/Chrome", #""outcome": "skipped", "reason": "owner-running", "pid": 4242, "name": "Google Chrome""#),
            try item("p1/node_modules", #""outcome": "skipped", "reason": "in-use", "pid": 77, "name": "node""#),
            try item("py1/.venv", #""outcome": "skipped", "reason": "changed-since-scan""#),
            try item("r1/target", #""outcome": "skipped", "reason": "kept""#),
            try item("com.docker.docker", action: "native-only", #""outcome": "native", "commands": [{"program": "docker", "args": ["--context", "desktop-linux", "image", "prune", "-f"], "success": true, "code": 0, "output": "Total reclaimed space: 2GB"}]"#),
            try item(".local/share/containers", action: "native-only", #""outcome": "native", "commands": [{"program": "podman", "args": ["system", "prune", "-f"], "success": false, "code": 125, "output": "Error: cannot connect to Podman\n"}]"#),
            try item(".ollama/models", action: "trash", #""outcome": "failed", "error": "Permission denied""#),
            try item(".npm/_cacache", #""outcome": "not-run""#),
        ]
        let json = """
            {"schema_version": 3, "ddc_version": "0.4.0", "cancelled": true, "failed": 2, "local_snapshots": 0,
             "journal": "\(data.directory.journal.path)", "last_scan_saved": true,
             "volumes": [{"mount_point": "/System/Volumes/Data", "free_before": 1000, "free_after": 1000}],
             "items": [\(items.joined(separator: ","))]}
            """
        return Data(json.utf8)
    }

    /// The eight kinds and `p2/node_modules`, which the clean does not list, selected.
    private func selectEight() throws {
        review.selection = Set(try [
            "Google/Chrome", "p1/node_modules", "py1/.venv", "r1/target", "com.docker.docker", ".local/share/containers",
            ".ollama/models", ".npm/_cacache", "p2/node_modules",
        ].map(id))
        #expect(review.selection.count == 9, "every one of them is selectable")
    }

    /// The clean of the selection, through the sheet, until its result shows.
    private func clean() async throws -> CleanResult {
        try fake.put("clean-report.json", try eightKinds())
        try fake.open("clean-start")
        try fake.open("clean-go")
        review.openSheet()
        try await until("the dry run") { review.sheet?.warnings == .checked }
        review.cleanConfirmedItems()
        try await until("the result") { review.sheet?.stage.result != nil }
        return try #require(review.sheet?.stage.result)
    }

    @Test("of the eight kinds only owner-running, in-use and not-run stay selected; an item the clean did not list keeps its tick")
    func selection() async throws {
        try selectEight()
        _ = try await clean()
        try await until("the reload of the saved report") { events.busy == [true, false] }
        #expect(events.reloads == 1)
        review.closeSheet()
        #expect(review.selection == Set(try ["Google/Chrome", "p1/node_modules", ".npm/_cacache", "p2/node_modules"].map(id)))
    }

    @Test("the result counts what stays selected, says what clears it, and offers Scan Again for the changed item; with nothing kept, no count line")
    func resultLines() async throws {
        try selectEight()
        let result = try await clean()
        #expect(result.staysSelected == 3)
        #expect(result.needsRescan == 1)
        #expect(CleanWording.staysSelected(result) == "3 items stay selected for the next clean.")
        #expect(CleanWording.resultAdvice(result)
            == "Quit Google Chrome, then clean again. In use by node; clean again when it ends. 1 item changed since the scan. Scan Again to see it as it is now.")

        let confirmation = DDCKit.Confirmation(items, selection: [], deletePermanently: false)
        func make(_ outcomes: [String]) throws -> CleanResult {
            let entries = outcomes.enumerated().map { index, outcome in
                #"{"path": "/Users/example/\#(index)", "dev": 1, "ino": \#(index), "rule": "cache.npm", "class": "safe", "action": "delete", "bytes": 10, \#(outcome)}"#
            }
            let json = #"{"schema_version": 3, "ddc_version": "0.4.0", "cancelled": false, "failed": 0, "local_snapshots": 0, "journal": "/j", "volumes": [], "items": [\#(entries.joined(separator: ","))]}"#
            return CleanResult.make(report: try CleanReport.read(Data(json.utf8)), confirmation: confirmation, volumes: [])
        }
        let none = try make([#""outcome": "deleted""#, #""outcome": "skipped", "reason": "changed-since-scan""#, #""outcome": "skipped", "reason": "kept""#])
        #expect(none.staysSelected == 0 && none.needsRescan == 1)
        #expect(CleanWording.staysSelected(none) == nil)
        #expect(CleanWording.resultAdvice(none) == "1 item changed since the scan. Scan Again to see it as it is now.")

        let one = try make([
            #""outcome": "skipped", "reason": "owner-running", "pid": 1, "name": "Google Chrome""#,
            #""outcome": "skipped", "reason": "owner-running", "pid": 2, "name": "Visual Studio Code""#,
            #""outcome": "deleted""#,
        ])
        #expect(CleanWording.staysSelected(one) == "2 items stay selected for the next clean.")
        #expect(CleanWording.resultAdvice(one) == "Quit Google Chrome and Visual Studio Code, then clean again.")
        #expect(one.needsRescan == 0)
        let single = try make([#""outcome": "skipped", "reason": "tool-not-ready", "program": "docker", "detail": "Docker Desktop is not running or not answering""#])
        #expect(CleanWording.staysSelected(single) == "1 item stays selected for the next clean.")
        #expect(CleanWording.resultAdvice(single)
            == "Start Docker Desktop, then clean again. If you no longer use Docker Desktop, its folder is yours to delete in Finder.")
        #expect(CleanWording.resultAdvice(try make([#""outcome": "skipped", "reason": "protected""#])) == nil, "the causes already name it")
    }

    @Test("after the prune the Docker item is deselected with a note that it was pruned, the failed prune says why, and a newer scan clears both")
    func toolNotes() async throws {
        try selectEight()
        _ = try await clean()
        try await until("the clean's end") { !review.isCleaning }
        review.closeSheet()
        let docker = try id("com.docker.docker")
        let podman = try id(".local/share/containers")
        #expect(!review.isSelected(docker) && !review.isSelected(podman))
        world.now += 180
        #expect(review.toolNote(for: docker) == "Pruned 3 min ago; Docker Desktop shrinks its disk image later")
        #expect(review.toolNote(for: podman) == "The prune failed: Error: cannot connect to Podman")
        #expect(review.toolNote(for: try id(".npm/_cacache")) == nil, "its tool did not run")

        // The report the clean saved, read again: the notes stay.
        review.reload()
        #expect(review.toolNote(for: docker) != nil)
        // A scan that started after the clean ended.
        world.snapshot = try ReviewFixture.snapshot(report: try Self.report(generatedAt: "2034-01-01T00:00:00Z"))
        await review.scanAgain()
        #expect(events.rescans == 1)
        #expect(review.toolNote(for: try ReviewFixture.id(try #require(review.items), "com.docker.docker")) == nil)
        #expect(review.toolNote(for: try ReviewFixture.id(try #require(review.items), ".local/share/containers")) == nil)
    }

    @Test("a reload for Settings before the reload after the clean keeps every tick; the reload after the clean applies the report once")
    func otherReloads() async throws {
        // The CLI could not save its report: the window waits for a scan.
        try fake.put("clean-report.json", Data(String(decoding: try eightKinds(), as: UTF8.self)
            .replacingOccurrences(of: #""last_scan_saved": true"#, with: #""last_scan_saved": false"#).utf8))
        try fake.open("clean-start")
        try fake.open("clean-go")
        events.holdRescan = true
        try selectEight()
        review.openSheet()
        try await until("the dry run") { review.sheet?.warnings == .checked }
        review.cleanConfirmedItems()
        try await until("the scan after the clean") { events.rescans == 1 }
        // Done while the scan runs: no reload yet.
        review.closeSheet()
        #expect(review.selection.count == 9)
        // Settings changed meanwhile: a reload that leaves the clean's report pending.
        try data.write(settings: #"{"schema_version": 1, "disabled_rules": ["cache.pip"], "min_size": 65536}"#)
        try FileManager.default.setAttributes([.modificationDate: Date().addingTimeInterval(5)], ofItemAtPath: data.directory.settings.path)
        review.refreshSettings()
        #expect(review.selection.count == 9)
        events.finishRescan()
        try await until("the reload after the clean") { !review.isCleaning }
        let kept = Set(try ["Google/Chrome", "p1/node_modules", ".npm/_cacache", "p2/node_modules"].map(id))
        #expect(review.selection == kept)
        // Applied once: the user's own ticks after it stay through the next reload.
        review.selection.insert(try id("py1/.venv"))
        review.reload()
        #expect(review.selection == kept.union([try id("py1/.venv")]))
    }

    @Test("the sheet keeps its last stage after it closes, so the dismiss never draws an empty panel")
    func lastSheet() async throws {
        try selectEight()
        let result = try await clean()
        try await until("the clean's end") { !review.isCleaning }
        review.closeSheet()
        #expect(review.sheet == nil)
        #expect(review.lastSheet?.stage == .result(result))

        // Back to Review from the confirm stage too.
        review.openSheet()
        let confirmation = try #require(review.sheet?.confirmation)
        review.closeSheet()
        #expect(review.sheet == nil)
        #expect(review.lastSheet?.stage == .confirm)
        #expect(review.lastSheet?.confirmation == confirmation)
    }
}
