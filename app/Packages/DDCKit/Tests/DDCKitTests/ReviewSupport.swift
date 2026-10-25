import Foundation
import Testing
@testable import DDCKit

/// The groups fixture as the Review window sees it. Its findings, by index:
/// 0 `.npm/_cacache` (safe), 1 `pip` (safe, its type switched off),
/// 2 `Chrome` (safe), 3 `.npm/_npx` (safe, under the minimum size),
/// 4 `p1/node_modules`, 5 `py1/.venv`, 6 `r1/target` (rebuildable, idle),
/// 7 `p2/node_modules` (rebuildable, active), 8 `.ollama/models` (review,
/// Trash), 9 `Downloads` and 10 `Pictures` (user data).
enum ReviewFixture {
    /// The window reads `settings.json` from the data directory: give it the fixture's.
    static func snapshot(report: Data? = nil, settingsIn data: TempDataDirectory? = nil) throws -> ReviewSnapshot {
        if let data { try data.write(settings: String(decoding: Fixture.data("scan-report-groups.settings"), as: UTF8.self)) }
        return ReviewSnapshot(report: try ScanReport.read(report ?? Fixture.data("scan-report-groups")), rules: try Fixture.rules())
    }

    static func items(report: Data? = nil, settings: Bool = true) throws -> ReviewItems {
        ReviewItems.make(
            report: try ScanReport.read(report ?? Fixture.data("scan-report-groups")),
            settings: settings ? try Fixture.groupsSettings() : nil,
            rules: try Fixture.rules()
        )
    }

    /// The row whose path ends with `suffix`.
    static func id(_ items: ReviewItems, _ suffix: String) throws -> Int {
        try #require(items.rows.first { $0.finding.path.hasSuffix(suffix) }).id
    }

    /// The selection the recorded dry run and clean report were made from:
    /// the default safe and rebuildable items, and the Ollama models.
    static func recordedSelection(_ items: ReviewItems) throws -> Set<Int> {
        items.defaultSelection(classes: [.safe, .rebuildable]).union([try id(items, ".ollama/models")])
    }

    /// A finding of the groups report with `changes` applied, as JSON.
    static func finding(_ index: Int, _ changes: [String: Any]) throws -> [String: Any] {
        let json = try #require(try JSONSerialization.jsonObject(with: Fixture.data("scan-report-groups")) as? [String: Any])
        var finding = try #require((json["findings"] as? [[String: Any]])?[index])
        finding.merge(changes) { _, new in new }
        return finding
    }
}

/// What the review model asked of the app: the icon's busy input, the
/// rescans and the reloads of a saved report.
@MainActor
final class ReviewEvents {
    var busy: [Bool] = []
    var rescans = 0
    /// Reads of the report a clean saved; `reloadFails` makes them fail.
    var reloads = 0
    var reloadFails = false
    var presented = 0
    var paused = false
    /// While set, a rescan waits until `finishRescan()`.
    var holdRescan = false
    private var held: CheckedContinuation<Void, Never>?

    func reloadReport() -> Bool {
        reloads += 1
        return !reloadFails
    }

    func rescan() async {
        rescans += 1
        if holdRescan { await withCheckedContinuation { held = $0 } }
    }

    func finishRescan() {
        holdRescan = false
        held?.resume()
        held = nil
    }
}

@MainActor
func makeReview(
    runner: (any CLIRunning)?,
    data: TempDataDirectory,
    defaults: TempDefaults,
    events: ReviewEvents = ReviewEvents(),
    clock: @escaping @MainActor () -> Date = { Date(timeIntervalSince1970: 2_000_000_000) },
    snapshot: @escaping @MainActor () -> ReviewSnapshot?
) -> ReviewModel {
    ReviewModel(
        runner: runner,
        data: data.directory,
        preferences: AppPreferences(defaults: defaults.defaults),
        snapshot: snapshot,
        present: {
            events.presented += 1
            return true
        },
        setBusy: { events.busy.append($0) },
        rescan: { await events.rescan() },
        reloadReport: { events.reloadReport() },
        paused: { events.paused },
        temporaryDirectory: data.url,
        clock: clock
    )
}

/// Waits until `condition` holds; a broken flow fails here instead of hanging.
@MainActor
func until(_ what: String = "condition", _ condition: @MainActor () -> Bool) async throws {
    for _ in 0..<1_000 {
        if condition() { return }
        try await Task.sleep(for: .milliseconds(10))
    }
    Issue.record("timed out waiting for \(what)")
    throw CancellationError()
}

extension ReviewModel.Stage {
    var isPreparing: Bool {
        if case .cleaning(.preparing) = self { return true }
        return false
    }

    var isCleaningPhase: Bool {
        if case .cleaning(.cleaning) = self { return true }
        return false
    }

    var result: CleanResult? {
        if case .result(let result) = self { return result }
        return nil
    }

    var stopped: (text: String, journal: URL?)? {
        if case .stopped(let text, let journal) = self { return (text, journal) }
        return nil
    }
}
