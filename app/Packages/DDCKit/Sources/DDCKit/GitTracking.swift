import Foundation
import Synchronization

/// Whether git tracks a `.ddc-keep`, for the Remove Protection confirmation's
/// warning (story "Protected by you: say what it is, and Remove Protection";
/// menu bar ADR §7, amended 2026-10-30): someone else may have committed the
/// marker, and removing it changes the checkout.
///
/// Only a real git binary at one of `binaries` is run, the first that exists.
/// The shim at /usr/bin/git and xcrun show an install dialog on a Mac without
/// the developer tools, so neither is ever run, and nothing asks xcode-select
/// where Xcode is: an Xcode elsewhere is not found. No git, a failure or a
/// run past `timeout` is `.unknown`, and the confirmation then warns of nothing.
public struct GitTracking: Sendable {
    public enum Answer: Sendable, Equatable {
        case tracked, untracked, unknown
    }

    /// Runs `program` with `arguments`: its exit status, or nil when it could
    /// not start, ended by a signal, or ran past `timeout` (it is then stopped).
    public typealias Run = @Sendable (_ program: String, _ arguments: [String], _ timeout: Duration) async -> Int32?

    /// Tried in order.
    public static let binaries = [
        "/Library/Developer/CommandLineTools/usr/bin/git",
        "/Applications/Xcode.app/Contents/Developer/usr/bin/git",
        "/opt/homebrew/bin/git",
        "/usr/local/bin/git",
    ]

    public static let timeout: Duration = .seconds(2)

    let binaries: [String]
    let run: Run

    /// Tests pass binaries of their own and a runner that runs nothing.
    public init(binaries: [String] = GitTracking.binaries, run: @escaping Run = GitTracking.launch) {
        self.binaries = binaries
        self.run = run
    }

    /// Asks nothing and runs nothing: every marker is `.unknown`. What a
    /// model gets unless the app hands it the real one.
    public static let off = GitTracking(binaries: [], run: { _, _, _ in nil })

    /// How long a git that ignored SIGTERM gets before SIGKILL.
    static let killAfter: Duration = .milliseconds(500)

    /// The first of `binaries` that is an executable file.
    public var binary: String? {
        binaries.first { FileManager.default.isExecutableFile(atPath: $0) }
    }

    /// Whether git tracks the `.ddc-keep` in `folder`. Asked only when a
    /// `.git` lies at or above `folder` within the scan root that holds it,
    /// and a git binary exists; otherwise nothing runs.
    public func marker(in folder: String, roots: [String]) async -> Answer {
        guard Self.inRepository(folder, roots: roots), let git = binary else { return .unknown }
        let arguments = ["-C", folder, "--no-optional-locks", "-c", "core.fsmonitor=false", "ls-files", "--error-unmatch", KeepMarker.name]
        return switch await run(git, arguments, Self.timeout) {
        case 0: .tracked
        // `--error-unmatch`: the path is not in the index.
        case 1: .untracked
        default: .unknown
        }
    }

    /// A `.git` (a folder, or a worktree's file) in `folder` or above it, up
    /// to the scan root that holds it. False outside every root.
    static func inRepository(_ folder: String, roots: [String]) -> Bool {
        guard let root = roots.filter({ folder == $0 || FolderList.isInside(folder, $0) }).max(by: { $0.count < $1.count }) else {
            return false
        }
        var dir = folder
        while true {
            var info = stat()
            if lstat(dir + "/.git", &info) == 0 { return true }
            guard dir != root, FolderList.isInside(dir, root) else { return false }
            dir = (dir as NSString).deletingLastPathComponent
        }
    }

    /// The app's runner: stdin, stdout and stderr to /dev/null, SIGTERM past
    /// the timeout, and SIGKILL `killAfter` later if it still runs, so a git
    /// that ignores SIGTERM cannot hold the confirmation. A run stopped that
    /// way is nil whatever its status.
    public static let launch: Run = { program, arguments, timeout in
        let process = Process()
        process.executableURL = URL(filePath: program)
        process.arguments = arguments
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        let timedOut = Flag()
        var watcher: Task<Void, Never>?
        let status: Int32? = await withCheckedContinuation { done in
            process.terminationHandler = { ended in
                done.resume(returning: ended.terminationReason == .exit ? ended.terminationStatus : nil)
            }
            do {
                try process.run()
            } catch {
                process.terminationHandler = nil
                done.resume(returning: nil)
                return
            }
            watcher = Task {
                try? await Task.sleep(for: timeout)
                guard !Task.isCancelled, process.isRunning else { return }
                timedOut.set()
                process.terminate()
                // Not cancelled from here on: the run ends only when the process does.
                try? await Task.sleep(for: GitTracking.killAfter)
                if process.isRunning { kill(process.processIdentifier, SIGKILL) }
            }
        }
        watcher?.cancel()
        return timedOut.isSet ? nil : status
    }

    /// Set once, from any thread.
    final class Flag: Sendable {
        private let value = Mutex(false)
        var isSet: Bool { value.withLock { $0 } }
        func set() { value.withLock { $0 = true } }
    }
}
