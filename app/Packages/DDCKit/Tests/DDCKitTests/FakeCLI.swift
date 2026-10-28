import Foundation
import Synchronization

/// A stand-in for `deepdrivecleaner`: a temporary directory per test, whose
/// `deepdrivecleaner` is a link to one shared script. macOS checks a newly
/// written executable on its first launch (about 0.6 s, one at a time), so a
/// fresh script per test made the parallel suite's first runs take seconds.
///
/// Every run appends `start <pid> <args>` and `end <pid> <args>` to a log.
/// Commands (the first argument):
/// - `wait <gate>`: block until the file `<gate>` exists in the gate directory;
/// - `exit <status>`: exit with that status;
/// - `flood`: 1 MiB to stderr, then 1 MiB to stdout, 64-byte lines;
/// - `report <file>`: print the file;
/// - `stderr <file>`: print the file to stderr;
/// - `crash`: kill itself with SIGABRT;
/// - `clean --items <file> [--dry-run] --json [--progress json]`: copies the
///   list to `items-<n>.json`. The dry run prints `dry-run.jsonl`, waiting for
///   gate `line-<i>` before line `i` when `dry-run.gated` exists, and exits
///   with `dry-run.exit` when that exists. The clean waits for `clean-start`,
///   then plays `clean-events-<k>.jsonl` to stderr for k = 1, 2, … while the
///   file exists, each after gate `events-<k>`, and waits for `clean-go`.
///   With `clean.crash` it kills itself with SIGABRT there. Then it copies
///   `saved-scan.json` over `lastScan`, as the CLI saves the report it
///   edited, and exits with `clean.exit` when that exists, else prints
///   `clean-report.json`. With `clean.trap`, SIGTERM prints the report and
///   then ends the script by the signal, as the CLI's handler does.
/// - `disk --json --progress json`: plays `disk-events.jsonl` to stderr when
///   it exists, waits for gate `disk-go`, and exits with `disk.exit` when that
///   exists. Else it copies `disk-report.json` over `lastDisk`, as the CLI
///   saves its report before it prints, and prints it. With `disk.trap`,
///   SIGTERM copies and prints `disk-stopped.json` when it exists, then ends
///   the script by the signal.
/// - `disk agents --json [--no-sizes]`: prints `agents-fast.json` for
///   `--no-sizes`, else `agents.json`. With `agents-fast.gated` (or
///   `agents.gated`) it first waits for gate `agents-fast-go` (`agents-go`),
///   and exits with `agents-fast.exit` (`agents.exit`) when that exists.
/// Gates replace sleeps, so the tests order runs without timing assumptions.
/// The directory is removed when the fake goes away. A gate gives up when the
/// directory is gone or after 30,000 polls (some minutes), so a test that fails or
/// leaks its fake never leaves a script polling for good.
final class FakeCLI: Sendable {
    let directory: URL
    let executable: URL
    let log: URL

    static let floodLines = 16_384  // 64-byte lines: 1 MiB per stream

    /// `lastScan`: the file a clean replaces, as the CLI saves the report it
    /// edited; `lastDisk`: the file `disk` writes.
    init(lastScan: URL? = nil, lastDisk: URL? = nil) throws {
        directory = FileManager.default.temporaryDirectory.appending(path: "ddckit-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        executable = directory.appending(path: "deepdrivecleaner")
        log = directory.appending(path: "runs.log")
        try FileManager.default.createSymbolicLink(at: executable, withDestinationURL: try Self.script.get())
        if let lastScan { try put("last-scan-path", Data(lastScan.path.utf8)) }
        if let lastDisk { try put("last-disk-path", Data(lastDisk.path.utf8)) }
    }

    /// The shared script, in a directory named after its text: written once
    /// per version, so macOS checks it once, and two checkouts never run each
    /// other's. `$0` is the test's link: the script finds its directory, and
    /// with it the log and the gates.
    private static let script = Result<URL, any Error> {
        let line63 = { (c: Character) in String(repeating: c, count: 63) }
        let script = """
            #!/bin/sh
            gates=$(dirname "$0")
            log="$gates/runs.log"
            last=$(cat "$gates/last-scan-path" 2>/dev/null)
            lastdisk=$(cat "$gates/last-disk-path" 2>/dev/null)
            echo "start $$ $*" >> "$log"
            # Its counter is its own: sh has no locals, and the dry run's
            # line loop counts in `i`.
            gate() {
                polls=0
                while [ ! -e "$gates/$1" ]; do
                    [ -d "$gates" ] && [ "$polls" -lt 30000 ] || { echo "end $$ gate $1 abandoned" >> "$log"; exit 70; }
                    polls=$((polls + 1)); sleep 0.01
                done
            }
            case "$1" in
              wait) gate "$2" ;;
              exit) echo "end $$ $*" >> "$log"; exit "$2" ;;
              flood) yes \(line63("e")) | head -n \(FakeCLI.floodLines) >&2
                     yes \(line63("o")) | head -n \(FakeCLI.floodLines) ;;
              report) cat "$2" ;;
              stderr) cat "$2" >&2 ;;
              crash) kill -ABRT $$ ;;
              clean)
                n=$(ls "$gates" | grep -c '^items-')
                cp "$3" "$gates/items-$n.json"
                if [ "$4" = "--dry-run" ]; then
                  i=0
                  while IFS= read -r line; do
                    [ -e "$gates/dry-run.gated" ] && gate "line-$i"
                    printf '%s\\n' "$line"
                    i=$((i + 1))
                  done < "$gates/dry-run.jsonl"
                  if [ -e "$gates/dry-run.exit" ]; then echo "end $$ $*" >> "$log"; exit "$(cat "$gates/dry-run.exit")"; fi
                else
                  [ -e "$gates/clean.trap" ] && trap 'cat "$gates/clean-report.json"; echo "end $$ $*" >> "$log"; trap - TERM; kill -TERM $$' TERM
                  gate clean-start
                  k=1
                  while [ -e "$gates/clean-events-$k.jsonl" ]; do
                    gate "events-$k"
                    cat "$gates/clean-events-$k.jsonl" >&2
                    k=$((k + 1))
                  done
                  gate clean-go
                  [ -e "$gates/clean.crash" ] && kill -ABRT $$
                  [ -e "$gates/saved-scan.json" ] && cp "$gates/saved-scan.json" "$last"
                  if [ -e "$gates/clean.exit" ]; then echo "end $$ $*" >> "$log"; exit "$(cat "$gates/clean.exit")"; fi
                  cat "$gates/clean-report.json"
                fi ;;
              disk)
                if [ "$2" = agents ]; then
                  a=agents; [ "$4" = --no-sizes ] && a=agents-fast
                  [ -e "$gates/$a.gated" ] && gate "$a-go"
                  if [ -e "$gates/$a.exit" ]; then echo "end $$ $*" >> "$log"; exit "$(cat "$gates/$a.exit")"; fi
                  cat "$gates/$a.json"
                  echo "end $$ $*" >> "$log"; exit 0
                fi
                if [ -e "$gates/disk.trap" ]; then
                  trap 'if [ -e "$gates/disk-stopped.json" ]; then cp "$gates/disk-stopped.json" "$lastdisk"; cat "$gates/disk-stopped.json"; fi; echo "end $$ $*" >> "$log"; trap - TERM; kill -TERM $$' TERM
                fi
                [ -e "$gates/disk-events.jsonl" ] && cat "$gates/disk-events.jsonl" >&2
                gate disk-go
                if [ -e "$gates/disk.exit" ]; then echo "end $$ $*" >> "$log"; exit "$(cat "$gates/disk.exit")"; fi
                cp "$gates/disk-report.json" "$lastdisk"
                cat "$gates/disk-report.json" ;;
            esac
            echo "end $$ $*" >> "$log"
            """
        // FNV-1a: stable across runs, unlike `Hasher`.
        let hash = script.utf8.reduce(UInt64(0xcbf2_9ce4_8422_2325)) { ($0 ^ UInt64($1)) &* 0x0000_0100_0000_01b3 }
        let url = FileManager.default.temporaryDirectory.appending(path: "ddckit-fake-cli-\(String(hash, radix: 16))/deepdrivecleaner")
        if !FileManager.default.isExecutableFile(atPath: url.path) {
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            try script.write(to: url, atomically: true, encoding: .utf8)
            try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: url.path)
        }
        return url
    }

    deinit {
        try? FileManager.default.removeItem(at: directory)
    }

    func open(_ gate: String) throws {
        try Data().write(to: directory.appending(path: gate))
    }

    /// Puts `data` in the fake's directory as `name` (a reply it replays).
    func put(_ name: String, _ data: Data) throws {
        try data.write(to: directory.appending(path: name))
    }

    /// The item lists the fake was given, in order.
    func lists() throws -> [Data] {
        try (0..<100).map { directory.appending(path: "items-\($0).json") }
            .filter { FileManager.default.fileExists(atPath: $0.path) }
            .map { try Data(contentsOf: $0) }
    }

    /// The log's lines as (event, pid, arguments).
    func runs() throws -> [(event: String, pid: String, args: String)] {
        guard FileManager.default.fileExists(atPath: log.path) else { return [] }
        return try String(contentsOf: log, encoding: .utf8).split(separator: "\n").map { line in
            let parts = line.split(separator: " ", maxSplits: 2).map(String.init)
            return (parts[0], parts[1], parts.count > 2 ? parts[2] : "")
        }
    }

    /// Waits until the log has a line `start … <args>`.
    func waitForStart(_ args: String) async throws {
        for _ in 0..<1_000 {
            if try runs().contains(where: { $0.event == "start" && $0.args == args }) { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        throw CancellationError()
    }
}

/// A counter the runner's `@Sendable` callbacks can share.
final class Counter: Sendable {
    private let value = Mutex(0)
    func increment() { value.withLock { $0 += 1 } }
    var count: Int { value.withLock { $0 } }
}

/// Lines the runner's `@Sendable` callbacks collect.
final class Lines: Sendable {
    private let value = Mutex<[String]>([])
    func append(_ line: String) { value.withLock { $0.append(line) } }
    var all: [String] { value.withLock { $0 } }
}
