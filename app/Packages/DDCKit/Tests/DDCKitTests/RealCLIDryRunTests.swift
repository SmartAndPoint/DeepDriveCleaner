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

    @Test("a picked download the window lists would go to the Trash once it is not recent; a fresh one is locked")
    func picksADownload() async throws {
        defer { try? FileManager.default.removeItem(at: root) }
        let downloads = root.appending(path: "home/Downloads")
        try FileManager.default.createDirectory(at: downloads, withIntermediateDirectories: true)
        // Above the 10 MiB floor of what the scan lists.
        for name in ["old.iso", "new.iso"] { try Data(count: 11 << 20).write(to: downloads.appending(path: name)) }
        try backdate(downloads.appending(path: "old.iso").path, to: Date(timeIntervalSinceNow: -86_400))

        let scan = await runner.run(.scan, ["scan", "--json"])
        #expect(scan.outcome == .done)
        let rulesRun = await runner.run(.rules, ["rules", "--json"])
        let rules = try RuleList.read(rulesRun.stdout)
        for rule in rules.rules where rule.class == .userData { #expect(CleanWording.userDataName(rule.id) != nil, "\(rule.id)") }
        let items = ReviewItems.make(report: try ScanReport.read(scan.stdout), settings: nil, rules: rules)
        let old = try #require(items.rows.first { $0.userEntry != nil && $0.finding.path.hasSuffix("/old.iso") })
        let new = try #require(items.rows.first { $0.userEntry != nil && $0.finding.path.hasSuffix("/new.iso") })
        #expect(old.selectable)
        #expect(new.lock == .entry(.recent), "the window shows it locked as of its scan")

        // The app never sends a locked row; the CLI is asked about it all the same.
        let fresh = ListedItem(path: new.finding.path, dev: new.finding.dev, ino: new.finding.ino, rule: new.finding.rule, class: .userData, action: .trash, permanent: false)
        let picked = DDCKit.Confirmation(items, selection: [old.id], deletePermanently: true).itemList(checkedAgainst: items)
        let file = root.appending(path: "picked.json")
        try ItemList(items: picked.items + [fresh]).encoded().write(to: file)
        let dry = await runner.run(.dryRun, ["clean", "--items", file.path, "--dry-run", "--json"])
        #expect(dry.outcome == .done, "\(dry.stderrTail.suffix(3))")
        let predictions = try String(decoding: dry.stdout, as: UTF8.self).split(separator: "\n").compactMap { line -> Prediction? in
            guard case .item(let item) = try PreflightLine.read(String(line)) else { return nil }
            return item.prediction
        }
        #expect(predictions == [.wouldTrash, .wouldSkip(.entryLocked(.recent))])
    }
}

/// Sets a file's modification time and date added, as an old download has
/// them (`setattrlist`, both attributes settable).
private func backdate(_ path: String, to date: Date) throws {
    var list = attrlist()
    list.bitmapcount = u_short(ATTR_BIT_MAP_COUNT)
    // ATTR_CMN_MODTIME, ATTR_CMN_ADDEDTIME: their timespecs in bit order.
    list.commonattr = attrgroup_t(0x0000_0400 | 0x1000_0000)
    let when = timespec(tv_sec: Int(date.timeIntervalSince1970), tv_nsec: 0)
    var times = [when, when]
    let status = times.withUnsafeMutableBytes { setattrlist(path, &list, $0.baseAddress, $0.count, UInt32(FSOPT_NOFOLLOW)) }
    guard status == 0 else { throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO) }
}
