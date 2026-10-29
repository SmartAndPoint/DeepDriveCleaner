import Foundation
import Testing
@testable import DDCKit

/// The CLI built by `cargo build` (`make check`); the test is skipped without it.
private let builtCLI = repositoryRoot().appending(path: "target/debug/deepdrivecleaner")

/// Story "Review window: after a clean, only skips that can clear stay
/// selected": the recording's loop, end to end with the real CLI. The CLI
/// runs over a temporary home and data directory with an empty `PATH`, set
/// by a wrapper script (the runner passes no environment), and is given
/// items whose action is delete only: nothing goes to the real Trash and no
/// tool runs.
@Suite("Review window: a real clean", .timeLimit(.minutes(1)), .enabled(if: FileManager.default.isExecutableFile(atPath: builtCLI.path)))
@MainActor
struct RealCLICleanTests {
    let root: URL
    let home: URL
    let data: DataDirectory
    let defaults: TempDefaults
    let runner: CLIRunner

    init() throws {
        root = FileManager.default.temporaryDirectory.appending(path: "ddckit-real-clean-\(UUID().uuidString)")
        home = root.appending(path: "home")
        let fm = FileManager.default
        // A cache (cache.npm) and a folder (python.pycache), both deleted.
        for file in [".npm/_cacache/content/x", "app/__pycache__/x.pyc"] { try Self.write(home.appending(path: file)) }
        data = DataDirectory(url: root.appending(path: "data"))
        try fm.createDirectory(at: data.url, withIntermediateDirectories: true)
        defaults = try TempDefaults()

        let wrapper = root.appending(path: "ddc")
        try """
            #!/bin/sh
            export HOME='\(home.path)'
            export DDC_DATA_DIR='\(data.url.path)'
            export PATH=''
            exec '\(builtCLI.path)' "$@"
            """.write(to: wrapper, atomically: true, encoding: .utf8)
        try fm.setAttributes([.posixPermissions: 0o755], ofItemAtPath: wrapper.path)
        runner = CLIRunner(executable: wrapper)
    }

    private static func write(_ file: URL) throws {
        try FileManager.default.createDirectory(at: file.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data(count: 2 << 20).write(to: file)
    }

    @Test("select a cache and a folder, change the folder after the scan, clean: the cache is gone, the folder is deselected, the result offers Scan Again, and the next check would delete it")
    func changedFolderLoop() async throws {
        defer { try? FileManager.default.removeItem(at: root) }
        let fm = FileManager.default
        let scan = await runner.run(.scan, ["scan", "--json"])
        #expect(scan.outcome == .done, "\(scan.stderrTail.suffix(3))")
        let rules = try RuleList.read(await runner.run(.rules, ["rules", "--json"]).stdout)
        let data = data
        let runner = runner
        let review = ReviewModel(
            runner: runner,
            data: data,
            preferences: AppPreferences(defaults: defaults.defaults),
            // The popover's report: the file the CLI saved last.
            snapshot: { (try? Data(contentsOf: data.lastScan)).flatMap { try? ScanReport.read($0) }.map { ReviewSnapshot(report: $0, rules: rules) } },
            present: { true },
            rescan: { _ = await runner.run(.scan, ["scan", "--json"]) },
            reloadReport: { true },
            temporaryDirectory: root,
            home: home.path
        )
        _ = review.open(ReviewRequest(group: nil, classes: [.safe], confirm: false))
        let items = try #require(review.items)
        let cache = try #require(items.rows.first { $0.finding.path.hasSuffix("/.npm/_cacache") })
        let folder = try #require(items.rows.first { $0.finding.path.hasSuffix("/app/__pycache__") })
        review.selection = [cache.id, folder.id]

        // The folder changes after the scan: renamed aside, made anew.
        try fm.moveItem(at: home.appending(path: "app/__pycache__"), to: root.appending(path: "aside"))
        try Self.write(home.appending(path: "app/__pycache__/x.pyc"))

        review.openSheet()
        // Delete only: never the real Trash, never a tool.
        try #require(review.sheet?.confirmation.items.map(\.effectiveAction) == [.delete, .delete])
        try await waitFor("the dry run") { review.sheet?.warnings == .checked }
        #expect(review.sheet?.predictions[folder.id] == .wouldSkip(.changedSinceScan))
        #expect(review.sheet?.predictions[cache.id] == .wouldDelete)
        review.cleanConfirmedItems()
        try await waitFor("the result") { review.sheet?.stage.result != nil || review.sheet?.stage.stopped != nil }
        let result = try #require(review.sheet?.stage.result, "\(String(describing: review.sheet?.stage))")
        #expect(result.deleted.count == 1)
        #expect(result.skipped.count == 1)
        #expect(result.needsRescan == 1, "the result offers Scan Again")
        #expect(result.staysSelected == 0)
        #expect(CleanWording.staysSelected(result) == nil)
        try await waitFor("the clean's end") { !review.isCleaning }
        review.closeSheet()

        #expect(!fm.fileExists(atPath: cache.finding.path))
        let after = try #require(review.items)
        #expect(!after.rows.contains { $0.finding.path == cache.finding.path && $0.selectable }, "the cache is gone")
        let changed = try #require(after.rows.first { $0.finding.path == folder.finding.path })
        #expect(changed.finding.ino != folder.finding.ino, "the saved report holds the folder as it is now")
        #expect(review.selection.isEmpty, "the changed folder is deselected")

        // The loop is broken: selected again, the next clean would delete it.
        review.selection = [changed.id]
        review.openSheet()
        try await waitFor("the second dry run") { review.sheet?.warnings == .checked }
        #expect(review.sheet?.predictions[changed.id] == .wouldDelete)
        review.closeSheet()
        #expect(fm.fileExists(atPath: folder.finding.path), "a dry run only")
    }
}

/// `until` with the suite's minute: the real CLI walks the listed trees,
/// which takes longer than a fake's reply on a busy machine.
@MainActor
private func waitFor(_ what: String, _ condition: @MainActor () -> Bool) async throws {
    for _ in 0..<5_000 {
        if condition() { return }
        try await Task.sleep(for: .milliseconds(10))
    }
    Issue.record("timed out waiting for \(what)")
    throw CancellationError()
}
