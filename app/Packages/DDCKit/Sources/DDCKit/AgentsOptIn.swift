import Foundation

/// "Let coding agents clean safe items" (story "Settings: let coding agents
/// clean safe items"; ADR "Agent footprint", §7, note of 2026-11-15; menu
/// bar ADR §6 item 6): the only code that runs `settings set`. The app never
/// writes `agents_clean_safe` from its own state: it runs the CLI, the one
/// writer that changes the key, then reads the file again. The toggle, with
/// its risk text beside it, is the person's consent, so `on` passes
/// `--from-app`; `off` needs nothing.
public enum AgentsOptIn {
    nonisolated public static let title = "Let coding agents clean safe items"
    nonisolated public static let help =
        "A coding agent can then remove safe build caches, like __pycache__, inside its own project without asking you. Never a .venv, a worktree or a shared cache. They come back at the next build."
    nonisolated public static let risk = "This lets a program remove files without asking you each time."
    nonisolated public static let olderDdc =
        "While this is on, a ddc older than this app stops reading settings.json until you update it."
    /// The CLI's `disclaimer::URL`.
    nonisolated public static let disclaimerURL = URL(string: "https://github.com/SmartAndPoint/DeepDriveCleaner/blob/main/DISCLAIMER.md")!
    /// The popover's line while the key is on.
    nonisolated public static let popoverLine = "Coding agents may clean safe items"

    static func arguments(_ on: Bool) -> [String] {
        on ? ["settings", "set", "agents_clean_safe", "on", "--from-app"] : ["settings", "set", "agents_clean_safe", "off"]
    }

    /// Runs the switch: nil when it is done, else what to show under the
    /// toggle (the CLI's last stderr line, or why it did not run).
    static func run(_ on: Bool, runner: any CLIRunning) async -> String? {
        let result = await runner.run(.settingsSet, arguments(on))
        if result.outcome == .done { return nil }
        if let line = result.stderrTail.last(where: { !$0.isEmpty }) { return line }
        return "The command-line tool could not switch it (\(result.outcome))."
    }
}
