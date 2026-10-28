import Foundation

/// The report of `deepdrivecleaner disk free --json` (the contract is
/// `crates/ddc-core/src/free/model.rs`, schema in
/// `docs/disk-free.schema.json`; story "Whole disk: free space with the
/// owning tool"). Strict like `DiskReport`: another schema version is
/// unreadable, and an unknown enum value is a decoding error.
public struct FreeReport: Decodable, Sendable, Equatable {
    /// `FREE_SCHEMA_VERSION` of the CLI this app embeds.
    public static let schemaVersion = 1

    public let schemaVersion: Int
    public let ddcVersion: String
    /// `dry_run`; named apart from the runner's `.dryRun`, the clean's.
    public let isDryRun: Bool
    /// One per listed action, in the list's order.
    public let actions: [ActionReport]
    /// `actions.jsonl`; nil in a dry run.
    public let journal: String?
    public let lastScan: ReportEdit
    public let lastDisk: ReportEdit

    // The decoder converts the keys to camel case before it matches them.
    private enum CodingKeys: String, CodingKey {
        case schemaVersion, ddcVersion, actions, journal, lastScan, lastDisk
        case isDryRun = "dryRun"
    }

    public static func read(_ data: Data) throws -> FreeReport {
        let decoder = JSONDecoder.snakeCase
        let version = try decoder.decode(VersionHeader.self, from: data).schemaVersion
        guard version == schemaVersion else { throw ScanReport.ReadError.otherSchema(version) }
        return try decoder.decode(FreeReport.self, from: data)
    }

    public enum ReportEdit: String, Decodable, Sendable, CaseIterable {
        case edited, unchanged, skipped, failed
    }

    public struct ActionReport: Decodable, Sendable, Equatable {
        public let action: DiskReport.ActionId
        /// `simulator-runtimes` or `homebrew`.
        public let area: String
        public let identifier: String?
        /// The program and its arguments, as run (or as it would run).
        public let command: [String]
        /// `NAME=value`, added to the environment.
        public let env: [String]
        /// The same command for a person to paste into Terminal.
        public let terminalCommand: String
        public let result: Outcome
        public let bytesBefore: UInt64?
        public let bytesAfter: UInt64?
        /// The last part of the tool's output; nil when it printed nothing.
        public let output: String?
        /// The tool's own dry run; only in a dry run.
        public let preview: Preview?

        /// What the folders lost, when both sizes were measured.
        public var freedBytes: UInt64? {
            guard let bytesBefore, let bytesAfter else { return nil }
            return bytesBefore > bytesAfter ? bytesBefore - bytesAfter : 0
        }
    }

    /// The CLI's `ActionResult`, tagged by `status`.
    public enum Outcome: Decodable, Sendable, Equatable {
        case freed
        case removedNotFreed
        case stillDeleting
        case refused(Refusal)
        case failed(needsAdmin: Bool)
        case wouldRun
        case notRun

        /// Every `status` the CLI writes.
        public static let tags = ["freed", "removed-not-freed", "still-deleting", "refused", "failed", "would-run", "not-run"]

        // The decoder converts `needs_admin` to `needsAdmin` before it matches a key.
        private enum CodingKeys: String, CodingKey {
            case status, reason, needsAdmin
        }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            let status = try c.decode(String.self, forKey: .status)
            switch status {
            case "freed": self = .freed
            case "removed-not-freed": self = .removedNotFreed
            case "still-deleting": self = .stillDeleting
            case "refused": self = .refused(try c.decode(Refusal.self, forKey: .reason))
            case "failed": self = .failed(needsAdmin: try c.decode(Bool.self, forKey: .needsAdmin))
            case "would-run": self = .wouldRun
            case "not-run": self = .notRun
            default:
                throw DecodingError.dataCorruptedError(forKey: .status, in: c, debugDescription: "unknown status \(status)")
            }
        }
    }

    public enum Refusal: String, Decodable, Sendable, CaseIterable {
        case unknownRuntime = "unknown-runtime"
        case notUnderAssets = "not-under-assets"
        case notDeletable = "not-deletable"
        case deleting
        case noRuntimeIdentifier = "no-runtime-identifier"
        case booted
        case devicesUnknown = "devices-unknown"
        case noAssetFolder = "no-asset-folder"
        case noDeveloperTools = "no-developer-tools"
        case firstLaunchPending = "first-launch-pending"
        case simctlFailed = "simctl-failed"
        case brewMissing = "brew-missing"
        case prefixMismatch = "prefix-mismatch"
        case cellarOutside = "cellar-outside"
        case cacheMismatch = "cache-mismatch"

        /// Why nothing ran, as a sentence's end: "It was not run: …".
        public var text: String {
            switch self {
            case .unknownRuntime: "no runtime with this identifier is listed any more"
            case .notUnderAssets: "its mount path is not under /System/Library/AssetsV2"
            case .notDeletable: "simctl lists it as not deletable"
            case .deleting: "it is already being deleted"
            case .noRuntimeIdentifier: "it has no runtime identifier, so its simulators cannot be checked"
            case .booted: "a simulator on it is running or not yet shut down"
            case .devicesUnknown: "its simulators could not be listed"
            case .noAssetFolder: "its folder under AssetsV2 is not known, so its size cannot be measured"
            case .noDeveloperTools: "Xcode's developer tools are not installed"
            case .firstLaunchPending: "Xcode has not finished its first launch"
            case .simctlFailed: "simctl failed"
            case .brewMissing: "/opt/homebrew/bin/brew is missing or did not answer"
            case .prefixMismatch: "brew --prefix is not /opt/homebrew"
            case .cellarOutside: "brew --cellar is not inside /opt/homebrew"
            case .cacheMismatch: "brew --cache is not ~/Library/Caches/Homebrew"
            }
        }
    }

    public struct Preview: Decodable, Sendable, Equatable {
        /// The tool's dry-run output, line by line.
        public let lines: [String]
        /// A runtime's measured folder, or what `brew cleanup -n` says it frees.
        public let estimateBytes: UInt64?
    }
}

/// One confirmed action of `disk free --actions` (`ListedAction` in
/// `free/model.rs`). The CLI refuses unknown keys.
public struct ListedAreaAction: Encodable, Sendable, Equatable, Hashable {
    public let action: DiskReport.ActionId
    /// The runtime's asset id; only, and always, on `simctl-runtime-delete`.
    public let identifier: String?

    public init(action: DiskReport.ActionId, identifier: String?) {
        self.action = action
        self.identifier = identifier
    }

    private enum CodingKeys: String, CodingKey {
        case action, identifier
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(action.rawValue, forKey: .action)
        try c.encodeIfPresent(identifier, forKey: .identifier)
    }
}

/// The input of `disk free --actions <file>`: what the sheet confirmed
/// (menu bar ADR §6 item 11).
public struct AreaActionList: Encodable, Sendable, Equatable {
    /// `FREE_SCHEMA_VERSION` of the CLI this app embeds.
    public static let schemaVersion = 1

    public let actions: [ListedAreaAction]

    public init(actions: [ListedAreaAction]) {
        self.actions = actions
    }

    private enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case actions
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(Self.schemaVersion, forKey: .schemaVersion)
        try c.encode(actions, forKey: .actions)
    }

    /// Sorted keys: one list always gives the same bytes.
    public func encoded() throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.withoutEscapingSlashes, .sortedKeys]
        return try encoder.encode(self)
    }
}
