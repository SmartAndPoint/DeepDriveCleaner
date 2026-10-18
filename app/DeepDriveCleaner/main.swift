import AppKit

// `--tick`, the scheduler's fallback program (the ADR's Open Questions): hand the
// work to an instance LaunchServices starts, so that instance, not this
// launchd-started process, is responsible for the scan. This runs and exits
// before any NSApplication exists: once AppKit registers this process as the
// running instance of the bundle id, `open -b` would only reopen it.
if CommandLine.arguments.contains("--tick") {
    let bundleID = Bundle.main.bundleIdentifier ?? "com.smartandpoint.deepdrivecleaner"
    let open = Process()
    open.executableURL = URL(filePath: "/usr/bin/open")
    open.arguments = ["-gjb", bundleID, "--args", "--scheduled"]
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
