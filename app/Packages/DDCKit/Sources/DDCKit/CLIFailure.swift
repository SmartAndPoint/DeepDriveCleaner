/// One sentence for a run of the CLI that did not end well, for the popover's
/// notice slot. The CLI writes its error last on stderr as `ddc: <message>`
/// (`crates/ddc-cli/src/main.rs`); a malformed `settings.json` and settings
/// roots that do not exist both end that way, with exit 1.
public enum CLIFailure {
    public static func message(_ outcome: RunOutcome, stderrTail: [String]) -> String? {
        switch outcome {
        case .done:
            return nil
        case .error(let status):
            let error = stderrTail.last { $0.hasPrefix("ddc: ") && !$0.hasPrefix("ddc: warning:") }
            return error.map { String($0.dropFirst("ddc: ".count)) } ?? "The scan failed (exit \(status))."
        case .busy:
            return "A clean is running; scan again when it ends."
        case .refused:
            return "The CLI refused the scan (exit 2): a bug in the app."
        case .cancelled:
            return "The scan was cancelled."
        case .crashed(let signal):
            return "The scan stopped unexpectedly (signal \(signal))."
        case .notLaunched(let reason):
            return "The scan could not start: \(reason)."
        }
    }
}
