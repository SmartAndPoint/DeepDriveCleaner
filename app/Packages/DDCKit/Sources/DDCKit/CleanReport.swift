import Foundation

/// Why the executor left an item alone (`SkipReason` in
/// `crates/ddc-core/src/exec/report.rs`, schema in `docs/clean.schema.json`).
/// Tagged by `reason`; a reason this app does not know is a decoding error.
public enum SkipReason: Sendable, Equatable, Hashable {
    case missing
    case notADirectory
    case symlink
    case changedSinceScan
    case parentChanged
    case ruleMismatch
    case outsideScanRoots
    case scanRoot
    case protected
    case projectRoot
    case kept
    case inUse(pid: Int64, name: String)
    case inUseUnknownCwd(pid: Int64, name: String)
    case ownerRunning(pid: Int64, name: String)
    case needsConfirmation
    case actionNotAllowed
    case incomplete
    case nativeUnavailable(program: String)
    /// The runtime did not answer before its prune commands; `detail` says why.
    case toolNotReady(program: String, detail: String)
    /// A picked file or folder is no longer one of its folder's entries.
    case notAnEntry
    /// A picked entry is locked now.
    case entryLocked(EntryLock)
    /// Inside a picked folder that went to the Trash.
    case insideAnotherItem

    /// Every `reason` value, as the schema lists them.
    public static let tags = [
        "missing", "not-a-directory", "symlink", "changed-since-scan", "parent-changed", "rule-mismatch",
        "outside-scan-roots", "scan-root", "protected", "project-root", "kept", "in-use", "in-use-unknown-cwd",
        "owner-running", "needs-confirmation", "action-not-allowed", "incomplete", "native-unavailable",
        "tool-not-ready", "not-an-entry", "entry-locked", "inside-another-item",
    ]

    enum CodingKeys: String, CodingKey {
        case reason, pid, name, program, detail, lock
    }

    /// Reads the reason from the container of the item it is flattened into.
    init(from c: KeyedDecodingContainer<CodingKeys>) throws {
        switch try c.decode(String.self, forKey: .reason) {
        case "missing": self = .missing
        case "not-a-directory": self = .notADirectory
        case "symlink": self = .symlink
        case "changed-since-scan": self = .changedSinceScan
        case "parent-changed": self = .parentChanged
        case "rule-mismatch": self = .ruleMismatch
        case "outside-scan-roots": self = .outsideScanRoots
        case "scan-root": self = .scanRoot
        case "protected": self = .protected
        case "project-root": self = .projectRoot
        case "kept": self = .kept
        case "in-use": self = .inUse(pid: try c.decode(Int64.self, forKey: .pid), name: try c.decode(String.self, forKey: .name))
        case "in-use-unknown-cwd":
            self = .inUseUnknownCwd(pid: try c.decode(Int64.self, forKey: .pid), name: try c.decode(String.self, forKey: .name))
        case "owner-running":
            self = .ownerRunning(pid: try c.decode(Int64.self, forKey: .pid), name: try c.decode(String.self, forKey: .name))
        case "needs-confirmation": self = .needsConfirmation
        case "action-not-allowed": self = .actionNotAllowed
        case "incomplete": self = .incomplete
        case "native-unavailable": self = .nativeUnavailable(program: try c.decode(String.self, forKey: .program))
        case "tool-not-ready":
            self = .toolNotReady(program: try c.decode(String.self, forKey: .program), detail: try c.decode(String.self, forKey: .detail))
        case "not-an-entry": self = .notAnEntry
        case "entry-locked": self = .entryLocked(try c.decode(EntryLock.self, forKey: .lock))
        case "inside-another-item": self = .insideAnotherItem
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .reason, in: c, debugDescription: "unknown reason \(other)")
        }
    }
}

/// `clean --items --json`: what happened to each listed item.
public struct CleanReport: Decodable, Sendable, Equatable {
    /// `CLEAN_SCHEMA_VERSION` of the CLI this app embeds.
    public static let schemaVersion = 3

    public enum ReadError: Error, Equatable {
        case otherSchema(Int)
    }

    public let schemaVersion: Int
    public let ddcVersion: String
    /// A signal stopped the run; the items it left are `notRun`.
    public let cancelled: Bool
    /// In the order of the list.
    public let items: [Item]
    /// Items that failed, or whose tool command failed.
    public let failed: UInt64
    public let volumes: [VolumeSpace]
    /// Local Time Machine snapshots keep deleted blocks until macOS thins them.
    public let localSnapshots: Int
    /// The action journal every started item was recorded in.
    public let journal: String
    /// `last-scan.json` was edited by what this clean did and saved, so no
    /// scan needs to follow (story "Faster clean: re-check only what the
    /// selection needs"). Absent from an older CLI: false.
    public let lastScanSaved: Bool

    private enum CodingKeys: String, CodingKey {
        case schemaVersion, ddcVersion, cancelled, items, failed, volumes, localSnapshots, journal, lastScanSaved
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try c.decode(Int.self, forKey: .schemaVersion)
        ddcVersion = try c.decode(String.self, forKey: .ddcVersion)
        cancelled = try c.decode(Bool.self, forKey: .cancelled)
        items = try c.decode([Item].self, forKey: .items)
        failed = try c.decode(UInt64.self, forKey: .failed)
        volumes = try c.decode([VolumeSpace].self, forKey: .volumes)
        localSnapshots = try c.decode(Int.self, forKey: .localSnapshots)
        journal = try c.decode(String.self, forKey: .journal)
        lastScanSaved = try c.decodeIfPresent(Bool.self, forKey: .lastScanSaved) ?? false
    }

    public struct Item: Decodable, Sendable, Equatable {
        public let path: String
        public let dev: UInt64
        public let ino: UInt64
        public let rule: String
        public let `class`: ScanReport.RiskClass
        public let action: ListedAction
        public let permanent: Bool
        /// Allocated bytes the CLI's rescan measured; nil when it refused the item before running it.
        public let bytes: UInt64?
        public let outcome: Outcome

        private enum CodingKeys: String, CodingKey {
            case path, dev, ino, rule, `class`, action, permanent, bytes
        }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            path = try c.decode(String.self, forKey: .path)
            dev = try c.decode(UInt64.self, forKey: .dev)
            ino = try c.decode(UInt64.self, forKey: .ino)
            rule = try c.decode(String.self, forKey: .rule)
            `class` = try c.decode(ScanReport.RiskClass.self, forKey: .class)
            action = try c.decode(ListedAction.self, forKey: .action)
            permanent = try c.decodeIfPresent(Bool.self, forKey: .permanent) ?? false
            bytes = try c.decodeIfPresent(UInt64.self, forKey: .bytes)
            outcome = try Outcome(from: decoder)
        }
    }

    public enum Outcome: Sendable, Equatable {
        case deleted
        case trashed
        /// The tool's own commands ran; one with `success: false` failed or was interrupted.
        case native([CommandResult])
        case skipped(SkipReason)
        case failed(String)
        /// Left by a cancel before it started.
        case notRun

        private enum CodingKeys: String, CodingKey {
            case outcome, commands, error
        }

        /// Reads the outcome flattened into an item: the report's, or a progress event's.
        init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            switch try c.decode(String.self, forKey: .outcome) {
            case "deleted": self = .deleted
            case "trashed": self = .trashed
            case "native": self = .native(try c.decode([CommandResult].self, forKey: .commands))
            case "skipped": self = .skipped(try SkipReason(from: decoder.container(keyedBy: SkipReason.CodingKeys.self)))
            case "failed": self = .failed(try c.decode(String.self, forKey: .error))
            case "not-run": self = .notRun
            case let other:
                throw DecodingError.dataCorruptedError(forKey: .outcome, in: c, debugDescription: "unknown outcome \(other)")
            }
        }
    }

    public struct CommandResult: Decodable, Sendable, Equatable {
        public let program: String
        public let args: [String]
        public let success: Bool
        public let code: Int32?
        /// The last part of what the command printed; empty when it printed nothing.
        public let output: String

        private enum CodingKeys: String, CodingKey {
            case program, args, success, code, output
        }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            program = try c.decode(String.self, forKey: .program)
            args = try c.decode([String].self, forKey: .args)
            success = try c.decode(Bool.self, forKey: .success)
            code = try c.decodeIfPresent(Int32.self, forKey: .code)
            output = try c.decodeIfPresent(String.self, forKey: .output) ?? ""
        }
    }

    public struct VolumeSpace: Decodable, Sendable, Equatable {
        public let mountPoint: String
        /// Bytes available to the user.
        public let freeBefore: UInt64
        public let freeAfter: UInt64
    }

    public static func read(_ data: Data) throws -> CleanReport {
        let decoder = JSONDecoder.snakeCase
        let version = try decoder.decode(VersionHeader.self, from: data).schemaVersion
        guard version == schemaVersion else { throw ReadError.otherSchema(version) }
        return try decoder.decode(CleanReport.self, from: data)
    }
}

/// One line of `clean --items --dry-run --json`: an item's prediction, then
/// a summary. The lines carry no `schema_version`; they belong to the clean
/// schema, which `CleanReport.schemaVersion` pins.
public enum PreflightLine: Decodable, Sendable, Equatable {
    case item(PreflightItem)
    case summary

    private enum CodingKeys: String, CodingKey {
        case type
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        switch try c.decode(String.self, forKey: .type) {
        case "item": self = .item(try PreflightItem(from: decoder))
        case "summary": self = .summary
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .type, in: c, debugDescription: "unknown line type \(other)")
        }
    }

    public static func read(_ line: String) throws -> PreflightLine {
        try JSONDecoder.snakeCase.decode(PreflightLine.self, from: Data(line.utf8))
    }
}

public struct PreflightItem: Decodable, Sendable, Equatable {
    public let path: String
    public let rule: String
    public let `class`: ScanReport.RiskClass
    public let action: ListedAction
    /// Allocated bytes now; nil when the item was not walked.
    public let bytes: UInt64?
    public let prediction: Prediction

    private enum CodingKeys: String, CodingKey {
        case path, rule, `class`, action, bytes, prediction
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        path = try c.decode(String.self, forKey: .path)
        rule = try c.decode(String.self, forKey: .rule)
        `class` = try c.decode(ScanReport.RiskClass.self, forKey: .class)
        action = try c.decode(ListedAction.self, forKey: .action)
        bytes = try c.decodeIfPresent(UInt64.self, forKey: .bytes)
        switch try c.decode(String.self, forKey: .prediction) {
        case "would-delete": prediction = .wouldDelete
        case "would-trash": prediction = .wouldTrash
        case "would-run": prediction = .wouldRun
        case "would-skip": prediction = .wouldSkip(try SkipReason(from: decoder.container(keyedBy: SkipReason.CodingKeys.self)))
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .prediction, in: c, debugDescription: "unknown prediction \(other)")
        }
    }
}

public enum Prediction: Sendable, Equatable {
    case wouldDelete
    case wouldTrash
    /// The tool's command (installed now; the clean checks its location).
    case wouldRun
    case wouldSkip(SkipReason)
}
