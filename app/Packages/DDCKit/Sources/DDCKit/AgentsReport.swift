import Foundation

/// The report of `deepdrivecleaner disk agents --json` (the contract is
/// `crates/ddc-core/src/agents/model.rs`, schema in
/// `docs/agents.schema.json`; story "Coding agents in the Whole disk view").
///
/// The schema version is read first, as for `DiskReport`. ddc's own enums
/// are strict; the values ddc copies from a harness record (`status`,
/// `entrypoint`, `name`, `cwd`) are plain strings, since they are another
/// program's internals.
public struct AgentsReport: Decodable, Sendable, Equatable {
    /// `AGENTS_SCHEMA_VERSION` of the CLI this app embeds.
    public static let schemaVersion = 1

    public let schemaVersion: Int
    public let ddcVersion: String
    public let generatedAt: String
    public let durationMs: UInt64
    /// False under `--no-sizes`: every `bytes` is nil.
    public let sizes: Bool
    public let harnesses: [Harness]

    public static func read(_ data: Data) throws -> AgentsReport {
        let decoder = JSONDecoder.snakeCase
        let version = try decoder.decode(VersionHeader.self, from: data).schemaVersion
        guard version == schemaVersion else { throw ScanReport.ReadError.otherSchema(version) }
        return try decoder.decode(AgentsReport.self, from: data)
    }

    public enum HarnessID: String, Decodable, Sendable, CaseIterable {
        case claudeCode = "claude-code"
        case codex, cursor, opencode
    }

    public struct Harness: Decodable, Sendable, Equatable {
        public let id: HarnessID
        public let name: String
        public let running: [Running]
        public let folders: [Folder]
        public let bytes: UInt64?
        /// Claude Code and Codex only.
        public let sessions: Sessions?
    }

    public enum RunningKind: String, Decodable, Sendable, CaseIterable {
        case cli
        case appServer = "app-server"
        case app, other
    }

    public struct Running: Decodable, Sendable, Equatable {
        public let pid: Int32
        public let kind: RunningKind
    }

    public struct Folder: Decodable, Sendable, Equatable {
        public let path: String
        public let nested: Bool
        public let skipped: Bool
        public let bytes: UInt64?
        public let unreadable: UInt64
    }

    public enum RecordsState: String, Decodable, Sendable, CaseIterable {
        case readable, unknown
    }

    public enum UnknownReason: String, Decodable, Sendable, CaseIterable {
        case missing, unreadable, unparsed, skipped
    }

    public struct Sessions: Decodable, Sendable, Equatable {
        public let records: RecordsState
        public let reason: UnknownReason?
        /// Live first.
        public let list: [Session]
        public let transcripts: Transcripts
    }

    public enum SessionState: String, Decodable, Sendable, CaseIterable {
        case live, stale, unknown
    }

    public struct Session: Decodable, Sendable, Equatable {
        public let id: String?
        public let state: SessionState
        /// RFC 3339, from Claude Code's record.
        public let startedAt: String?
        /// The time in a Codex rollout's name, as written there.
        public let rolloutTime: String?
        public let status: String?
        public let entrypoint: String?
        public let name: String?
        public let cwd: String?
    }

    public struct Transcripts: Decodable, Sendable, Equatable {
        public let count: UInt64?
        public let bytes: UInt64?
        public let skipped: Bool
    }
}
