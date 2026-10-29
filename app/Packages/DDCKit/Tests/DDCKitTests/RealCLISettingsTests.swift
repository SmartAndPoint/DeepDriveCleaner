import Foundation
import Testing
@testable import DDCKit

/// The CLI built by `cargo build` (`make check`); the test is skipped without it.
private let builtCLI = repositoryRoot().appending(path: "target/debug/deepdrivecleaner")

/// Story "Settings: folders, types, notifications": what the Settings model
/// writes is what the real CLI reads. A scan only: it removes nothing. The
/// CLI runs over a temporary home and data directory, set by a wrapper
/// script (the runner passes no environment).
@Suite("Settings: the real CLI", .timeLimit(.minutes(1)), .enabled(if: FileManager.default.isExecutableFile(atPath: builtCLI.path)))
@MainActor
struct RealCLISettingsTests {
    let root: URL
    let home: URL
    let data: DataDirectory
    let runner: CLIRunner

    init() throws {
        root = FileManager.default.temporaryDirectory.appending(path: "ddckit-real-settings-\(UUID().uuidString)")
        home = root.appending(path: "home")
        let fm = FileManager.default
        // Two idle projects with node_modules: one in a skipped folder, one not.
        let old = Date(timeIntervalSince1970: 1_577_836_800)
        for project in ["archive/p1", "work/p2"] {
            let dir = home.appending(path: project)
            try fm.createDirectory(at: dir.appending(path: "node_modules/a"), withIntermediateDirectories: true)
            try Data(count: 2 << 20).write(to: dir.appending(path: "node_modules/a/index.js"))
            for file in ["package.json", "package-lock.json"] {
                try Data("{}".utf8).write(to: dir.appending(path: file))
                try fm.setAttributes([.modificationDate: old], ofItemAtPath: dir.appending(path: file).path)
            }
            try fm.setAttributes([.modificationDate: old], ofItemAtPath: dir.path)
        }
        data = DataDirectory(url: root.appending(path: "data"))
        try fm.createDirectory(at: data.url, withIntermediateDirectories: true)

        let wrapper = root.appending(path: "ddc")
        try """
            #!/bin/sh
            export HOME='\(home.path)'
            export DDC_DATA_DIR='\(data.url.path)'
            exec '\(builtCLI.path)' "$@"
            """.write(to: wrapper, atomically: true, encoding: .utf8)
        try fm.setAttributes([.posixPermissions: 0o755], ofItemAtPath: wrapper.path)
        runner = CLIRunner(executable: wrapper)
    }

    @Test("a skip written by the Settings model is not scanned, and the node_modules inside it is not found")
    func skipIsNotScanned() async throws {
        defer { try? FileManager.default.removeItem(at: root) }
        let defaults = try TempDefaults()
        let preferences = AppPreferences(defaults: defaults.defaults)
        let popover = PopoverModel(
            runner: runner,
            data: data,
            probe: FullDiskAccessProbe(path: root.appending(path: "TCC.db")),
            preferences: preferences,
            router: FakeRouter(),
            home: home.path
        )
        let settings = SettingsModel(store: SettingsStore(url: data.settings), popover: popover, preferences: preferences, home: home.path)
        let archive = home.appending(path: "archive").path
        settings.addExclusion(archive, mode: .skip)
        #expect(settings.folderMessage == nil)
        #expect(try SettingsFile.read(at: data.settings).skips == [archive])

        await popover.scanNow()
        #expect(popover.notices.isEmpty, "\(popover.notices.map(\.text))")
        let report = try #require(popover.loadedReport)
        #expect(report.notScanned == [FolderList.resolved(archive)], "canonical: /var is /private/var")
        let modules = report.findings.filter { $0.rule == "node.node_modules" }.map(\.path)
        #expect(modules.count == 1 && modules.allSatisfy { $0.hasSuffix("/work/p2/node_modules") })
        #expect(!report.findings.contains { $0.path.contains("/archive/") })
        let items = ReviewItems.make(report: report, settings: popover.settings, rules: popover.rules)
        #expect(items.notScanned == "Not scanned: ~/archive (Skip entirely in Settings)")
        #expect(!items.backgroundSkipped)
    }
}
