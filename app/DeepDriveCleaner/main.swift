import AppKit
import DDCKit

// `--tick`, the bundled scheduler agent's program (B6; the ADR's §5 note of
// 2026-10-28): hand the work to an instance LaunchServices starts, so that
// instance, not this launchd-started process, is responsible for the scan.
// It opens the app by its own bundle path, so a Debug copy and a release copy
// never launch each other. This runs and exits before any NSApplication
// exists: once AppKit registers this process as the running instance of the
// bundle, `open` would only reopen it.
if CommandLine.arguments.contains(LaunchMode.tickArgument) {
    let open = Process()
    open.executableURL = URL(filePath: "/usr/bin/open")
    open.arguments = LaunchMode.tickOpenArguments(bundleURL: Bundle.main.bundleURL)
    AppLog.write("tick: open \(open.arguments?.joined(separator: " ") ?? "")")
    do {
        try open.run()
        open.waitUntilExit()
        AppLog.write("tick: open exited \(open.terminationStatus)")
        AppLog.flush()
        exit(open.terminationStatus)
    } catch {
        AppLog.write("tick: cannot run open: \(error)")
        AppLog.flush()
        exit(1)
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.run()
