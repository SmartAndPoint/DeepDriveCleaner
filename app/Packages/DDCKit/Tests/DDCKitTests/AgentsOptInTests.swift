import Foundation
import Testing

@testable import DDCKit

/// A runner for `settings set`: records each call and, when it succeeds,
/// writes the file as the CLI would.
actor SwitchRunner: CLIRunning {
    private(set) var calls: [(CLIRunner.Kind, [String])] = []
    private let settings: URL
    private let outcome: RunOutcome
    private let stderr: [String]

    init(settings: URL, outcome: RunOutcome = .done, stderr: [String] = []) {
        self.settings = settings
        self.outcome = outcome
        self.stderr = stderr
    }

    func run(_ kind: CLIRunner.Kind, _ arguments: [String], options: CLIRunner.Options) async -> CLIRunner.Result {
        calls.append((kind, arguments))
        if outcome == .done, kind == .settingsSet {
            let on = arguments.contains("on")
            let text = on ? #"{"schema_version": 1, "agents_clean_safe": true}"# : #"{"schema_version": 1}"#
            try? Data(text.utf8).write(to: settings)
        }
        return CLIRunner.Result(outcome: outcome, stdout: Data(), stderrTail: stderr)
    }

    func cancel(_ ticket: CLIRunner.Ticket) async {}
}

/// Story "Settings: let coding agents clean safe items": the Automation
/// pane's toggle runs the CLI and shows the file as the CLI left it; the
/// popover names the opt-in while it is on.
@Suite("Coding agents' opt-in", .timeLimit(.minutes(1)))
@MainActor
struct AgentsOptInTests {
    let data: TempDataDirectory
    let defaults: TempDefaults

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
    }

    private func settings(_ runner: (any CLIRunning)?) throws -> SettingsModel {
        SettingsModel(
            store: SettingsStore(url: data.directory.settings),
            popover: try makeModel(runner: nil, data: data, defaults: defaults),
            preferences: AppPreferences(defaults: defaults.defaults),
            home: "/Users/example",
            runner: runner
        )
    }

    @Test("the toggle runs settings set once per switch, on with --approved, and then shows the file as the CLI left it")
    func toggleRunsTheCLI() async throws {
        let runner = SwitchRunner(settings: data.directory.settings)
        let model = try settings(runner)
        #expect(!model.agentsCleanSafe)

        await model.setAgentsCleanSafe(true)

        var calls = await runner.calls
        #expect(calls.count == 1)
        #expect(calls.first?.0 == .settingsSet)
        #expect(calls.first?.1 == ["settings", "set", "agents_clean_safe", "on", "--approved"])
        #expect(model.agentsCleanSafe)
        #expect(model.agentsOptInFailure == nil && !model.agentsOptInRunning)

        await model.setAgentsCleanSafe(false)

        calls = await runner.calls
        #expect(calls.count == 2)
        #expect(calls.last?.1 == ["settings", "set", "agents_clean_safe", "off"])
        #expect(!model.agentsCleanSafe)
    }

    @Test("a run that fails leaves the toggle as the file is and shows the CLI's last stderr line")
    func failureShowsTheCLI() async throws {
        let line = "ddc: another program is changing the settings; nothing changed, try again"
        let runner = SwitchRunner(settings: data.directory.settings, outcome: .error(status: 1), stderr: ["", line])
        let model = try settings(runner)

        await model.setAgentsCleanSafe(true)

        #expect(!model.agentsCleanSafe, "the file still says off")
        #expect(model.agentsOptInFailure == line)
        #expect(await runner.calls.count == 1)
    }

    @Test("without the command-line tool the toggle runs nothing and says why")
    func noCLI() async throws {
        let model = try settings(nil)
        await model.setAgentsCleanSafe(true)
        #expect(!model.agentsCleanSafe)
        #expect(model.agentsOptInFailure?.contains("missing") == true)
    }

    @Test("the popover's line shows only while settings.json holds the key")
    func popoverLine() async throws {
        let popover = try makeModel(runner: nil, data: data, defaults: defaults)
        await popover.refresh()
        #expect(popover.agentsOptInLine == nil, "no file")

        try data.write(settings: #"{"schema_version": 1, "agents_clean_safe": true}"#)
        try bump(1)
        await popover.refresh()
        #expect(popover.agentsOptInLine == AgentsOptIn.popoverLine)

        try data.write(settings: #"{"schema_version": 1}"#)
        try bump(2)
        await popover.refresh()
        #expect(popover.agentsOptInLine == nil, "off")

        try data.write(settings: #"{"schema_version": 1, "agents_clean_safe": true, "unknown": 1}"#)
        try bump(3)
        await popover.refresh()
        #expect(popover.agentsOptInLine == nil, "unreadable")
    }

    @Test("settings set runs in a lane of its own, beside the queue")
    func lane() {
        #expect(CLIRunner.Kind.settingsSet.hasLane)
    }

    /// A new modification time, so the popover reads the file again.
    private func bump(_ n: Double) throws {
        let later = Date(timeIntervalSince1970: 1_800_000_000 + n * 60)
        try FileManager.default.setAttributes([.modificationDate: later], ofItemAtPath: data.directory.settings.path)
    }
}
