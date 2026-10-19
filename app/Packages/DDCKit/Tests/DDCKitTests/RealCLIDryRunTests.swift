import Foundation
import Testing
@testable import DDCKit

/// The CLI built by `cargo build` (`make check`); the test is skipped without it.
private let builtCLI = repositoryRoot().appending(path: "target/debug/deepdrivecleaner")

/// Story "Review and clean window": the list the window builds is one the
/// real CLI accepts, for each action and class. A dry run only: it trashes,
/// deletes and runs nothing. The CLI runs over a temporary home and data
/// directory, set by a wrapper script (the runner passes no environment).
@Suite("Review window: the real CLI", .timeLimit(.minutes(1)), .enabled(if: FileManager.default.isExecutableFile(atPath: builtCLI.path)))
@MainActor
struct RealCLIDryRunTests {
    let root: URL
    let runner: CLIRunner

    init() throws {
        root = FileManager.default.temporaryDirectory.appending(path: "ddckit-real-\(UUID().uuidString)")
        let home = root.appending(path: "home")
        let fm = FileManager.default
        // delete (safe, rebuildable), native (safe), native-only (review), trash (review).
        for file in [
            ".npm/_cacache/content/x", "Library/Caches/go-build/aa/x", ".local/share/containers/storage/x",
            ".ollama/models/blobs/x", "p1/node_modules/a/index.js",
        ] {
            let url = home.appending(path: file)
            try fm.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try Data(count: 2 << 20).write(to: url)
        }
        let old = Date(timeIntervalSince1970: 1_577_836_800)  // 2020: the project is idle
        for file in ["p1/package.json", "p1/package-lock.json"] {
            try Data("{}".utf8).write(to: home.appending(path: file))
            try fm.setAttributes([.modificationDate: old], ofItemAtPath: home.appending(path: file).path)
        }
        try fm.setAttributes([.modificationDate: old], ofItemAtPath: home.appending(path: "p1").path)
        try fm.createDirectory(at: root.appending(path: "data"), withIntermediateDirectories: true)

        let wrapper = root.appending(path: "ddc")
        try """
            #!/bin/sh
            export HOME='\(home.path)'
            export DDC_DATA_DIR='\(root.appending(path: "data").path)'
            exec '\(builtCLI.path)' "$@"
            """.write(to: wrapper, atomically: true, encoding: .utf8)
        try fm.setAttributes([.posixPermissions: 0o755], ofItemAtPath: wrapper.path)
        runner = CLIRunner(executable: wrapper)
    }

    @Test("the dry run accepts the window's list for every selectable finding, Trash or permanent")
    func acceptsTheList() async throws {
        defer { try? FileManager.default.removeItem(at: root) }
        let scan = await runner.run(.scan, ["scan", "--json"])
        #expect(scan.outcome == .done)
        let rulesRun = await runner.run(.rules, ["rules", "--json"])
        let items = ReviewItems.make(report: try ScanReport.read(scan.stdout), settings: nil, rules: try RuleList.read(rulesRun.stdout))
        let selectable = Set(items.rows.filter(\.selectable).map(\.id))

        for permanent in [false, true] {
            let confirmation = DDCKit.Confirmation(items, selection: selectable, deletePermanently: permanent)
            let list = confirmation.itemList(checkedAgainst: items)
            let pairs = Set(list.items.map { "\($0.action.rawValue)/\($0.class.rawValue)" })
            let review = permanent ? "delete/review" : "trash/review"
            #expect(pairs == ["delete/safe", "native/safe", "delete/rebuildable", "native-only/review", review])
            #expect(list.items.contains { $0.permanent } == permanent)

            let file = root.appending(path: "items-\(permanent).json")
            try list.encoded().write(to: file)
            let dry = await runner.run(.dryRun, ["clean", "--items", file.path, "--dry-run", "--json"])
            #expect(dry.outcome == .done, "\(dry.stderrTail.suffix(3))")
            let predictions = try String(decoding: dry.stdout, as: UTF8.self).split(separator: "\n").compactMap { line -> Prediction? in
                guard case .item(let item) = try PreflightLine.read(String(line)) else { return nil }
                return item.prediction
            }
            #expect(predictions.count == list.items.count)
            for prediction in predictions {
                #expect(prediction != .wouldSkip(.changedSinceScan))
                #expect(prediction != .wouldSkip(.actionNotAllowed))
                #expect(prediction != .wouldSkip(.ruleMismatch))
            }
        }
    }
}
