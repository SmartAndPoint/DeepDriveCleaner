import Foundation
import Testing
@testable import DDCKit

/// Fixtures and fakes for the popover's tests. Everything lives in temporary
/// directories and `UserDefaults` suites: never the real data directory, the
/// app's defaults domain, or TCC.db.
enum Fixture {
    static func url(_ name: String) throws -> URL {
        try #require(Bundle.module.url(forResource: name, withExtension: "json", subdirectory: "Fixtures"))
    }

    static func data(_ name: String) throws -> Data {
        try Data(contentsOf: url(name))
    }

    /// A fixture that is not JSON (`clean-dry-run.jsonl`).
    static func url(_ name: String, extension ext: String) throws -> URL {
        try #require(Bundle.module.url(forResource: name, withExtension: ext, subdirectory: "Fixtures"))
    }

    /// The recorded report of a synthetic home (`scan-report-groups.json`):
    /// 11 findings in 7 groups, 3 of them ignored by its settings file.
    static func groupsReport() throws -> ScanReport {
        try ScanReport.read(data("scan-report-groups"))
    }

    /// The `settings.json` the groups report was scanned with.
    static func groupsSettings() throws -> SharedSettings {
        try SharedSettings.read(at: url("scan-report-groups.settings"))
    }

    static func rules() throws -> RuleList {
        try RuleList.read(data("rules"))
    }

    /// A fixture with `edit` applied to its JSON.
    static func edited(_ name: String, _ edit: (inout [String: Any]) -> Void) throws -> Data {
        var json = try #require(try JSONSerialization.jsonObject(with: data(name)) as? [String: Any])
        edit(&json)
        return try JSONSerialization.data(withJSONObject: json)
    }

    /// Edits the findings of a report fixture.
    static func editedFindings(_ name: String, _ edit: (inout [[String: Any]]) -> Void) throws -> Data {
        try edited(name) { json in
            var findings = json["findings"] as? [[String: Any]] ?? []
            edit(&findings)
            json["findings"] = findings
        }
    }
}

/// A temporary data directory, removed when the value goes away.
final class TempDataDirectory {
    let url: URL
    var directory: DataDirectory { DataDirectory(url: url) }

    init() throws {
        url = FileManager.default.temporaryDirectory.appending(path: "ddckit-data-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
    }

    deinit {
        try? FileManager.default.removeItem(at: url)
    }

    func write(lastScan data: Data) throws {
        try data.write(to: directory.lastScan)
    }

    func write(settings text: String) throws {
        try Data(text.utf8).write(to: directory.settings)
    }

    /// A path for the Full Disk Access probe: readable when `granted`, missing otherwise.
    func probe(granted: Bool) throws -> FullDiskAccessProbe {
        let path = url.appending(path: "TCC.db")
        if granted { try Data("x".utf8).write(to: path) }
        return FullDiskAccessProbe(path: path)
    }
}

/// A `UserDefaults` suite of its own, removed when the value goes away.
final class TempDefaults {
    let name = "ddckit-tests-\(UUID().uuidString)"
    let defaults: UserDefaults

    init() throws {
        defaults = try #require(UserDefaults(suiteName: name))
    }

    deinit {
        defaults.removePersistentDomain(forName: name)
    }
}

/// Records what the popover asks of the Review window.
@MainActor
final class FakeRouter: ReviewRouting {
    var requests: [ReviewRequest] = []
    /// What `open` answers: whether a Review window opened.
    var opens = false

    func open(_ request: ReviewRequest) -> Bool {
        requests.append(request)
        return opens
    }
}

/// A CLI that answers from memory. A scan blocks until `finishScan()`, so a
/// test can act while it runs.
actor FakeRunner: CLIRunning {
    private(set) var calls: [CLIRunner.Kind] = []
    private(set) var arguments: [[String]] = []
    private var scanResult: CLIRunner.Result
    /// stderr lines a scan writes before it blocks.
    private var scanLines: [String] = []
    private var rulesResult: CLIRunner.Result
    private var gate: CheckedContinuation<Void, Never>?
    private var released = false
    private let blocks: Bool
    /// Replies to the scheduler's kinds (`backgroundScan`, `notify`,
    /// `schedule`), each with what the run does first; done and empty by default.
    private var replies: [CLIRunner.Kind: (result: CLIRunner.Result, effect: @Sendable () -> Void)] = [:]

    init(scan: CLIRunner.Result, rules: CLIRunner.Result? = nil, blocks: Bool = false) {
        scanResult = scan
        rulesResult = rules ?? CLIRunner.Result(outcome: .error(status: 1), stdout: Data(), stderrTail: [])
        self.blocks = blocks
    }

    static func done(_ stdout: Data) -> CLIRunner.Result {
        CLIRunner.Result(outcome: .done, stdout: stdout, stderrTail: [])
    }

    func run(_ kind: CLIRunner.Kind, _ arguments: [String], options: CLIRunner.Options) async -> CLIRunner.Result {
        calls.append(kind)
        self.arguments.append(arguments)
        switch kind {
        case .rules:
            return rulesResult
        case .dryRun, .clean:
            // The review tests that run these use `FakeCLI`; here they only count.
            return CLIRunner.Result(outcome: .done, stdout: Data(), stderrTail: [])
        case .backgroundScan, .notify, .schedule:
            guard let reply = replies[kind] else { return Self.done(Data()) }
            reply.effect()
            return reply.result
        default:
            for line in scanLines { options.stderrLine?(line) }
            if blocks && !released {
                await withCheckedContinuation { gate = $0 }
            }
            return scanResult
        }
    }

    func cancel(_ ticket: CLIRunner.Ticket) async {}

    /// What runs of `kind` return, after `effect` (the uninstall deletes its plist).
    func reply(_ kind: CLIRunner.Kind, _ result: CLIRunner.Result, effect: @escaping @Sendable () -> Void = {}) {
        replies[kind] = (result, effect)
    }

    /// The arguments of every run of `kind`.
    func arguments(of kind: CLIRunner.Kind) -> [[String]] {
        zip(calls, arguments).filter { $0.0 == kind }.map(\.1)
    }

    /// What the next scans return.
    func reply(scan: CLIRunner.Result) {
        scanResult = scan
    }

    /// What the next scans write to stderr.
    func reply(scanLines: [String]) {
        self.scanLines = scanLines
    }

    func count(_ kind: CLIRunner.Kind) -> Int {
        calls.filter { $0 == kind }.count
    }

    func finishScan() {
        released = true
        gate?.resume()
        gate = nil
    }

    /// Waits until a scan is blocked at the gate.
    func waitForScan() async throws {
        for _ in 0..<1_000 {
            if gate != nil { return }
            try await Task.sleep(for: .milliseconds(5))
        }
        throw CancellationError()
    }
}

/// The repository root, from this file (`app/Packages/DDCKit/Tests/DDCKitTests/<file>`).
func repositoryRoot() -> URL {
    var root = URL(filePath: #filePath)
    for _ in 0..<6 { root.deleteLastPathComponent() }
    return root
}

func rustSource(_ path: String) throws -> String {
    try String(contentsOf: repositoryRoot().appending(path: path), encoding: .utf8)
}

@MainActor
func makeModel(
    runner: (any CLIRunning)?,
    data: TempDataDirectory,
    defaults: TempDefaults,
    router: FakeRouter = FakeRouter(),
    probeGranted: Bool = true,
    nextScan: @escaping @MainActor () -> NextScan = { .off },
    legacyAgentFound: @escaping @MainActor () -> Bool = { false },
    clock: @escaping @MainActor () -> Date = { Date(timeIntervalSince1970: 1_800_000_000) }
) throws -> PopoverModel {
    PopoverModel(
        runner: runner,
        data: data.directory,
        probe: try data.probe(granted: probeGranted),
        preferences: AppPreferences(defaults: defaults.defaults),
        router: router,
        nextScan: nextScan,
        legacyAgentFound: legacyAgentFound,
        clock: clock
    )
}
