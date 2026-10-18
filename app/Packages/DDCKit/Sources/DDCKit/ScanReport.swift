import Foundation

/// The part of `deepdrivecleaner scan --json` the app reads (the contract is
/// `crates/ddc-core/src/model.rs`, schema in `docs/scan.schema.json`).
///
/// The schema version is read first: a report of another version is
/// unreadable as a whole, never half-decoded.
public struct ScanReport: Decodable, Sendable, Equatable {
    /// `SCHEMA_VERSION` of the CLI this app embeds.
    public static let schemaVersion = 1

    public enum ReadError: Error, Equatable {
        case otherSchema(Int)
    }

    public let schemaVersion: Int
    public let ddcVersion: String
    public let generatedAt: String
    public let durationMs: UInt64
    public let fullDiskAccess: Bool
    public let minAgeDays: UInt64
    public let totals: Totals
    public let findings: [Finding]
    public let errorsTotal: UInt64
    public let cancelled: Bool

    public struct Totals: Decodable, Sendable, Equatable {
        public let scannedBytes: UInt64
        public let files: UInt64
        public let dirs: UInt64
        public let classes: [ClassTotal]
        public let selectedBytes: UInt64
        public let selectedReclaimableBytes: UInt64
    }

    public struct ClassTotal: Decodable, Sendable, Equatable {
        public let `class`: RiskClass
        public let count: UInt64
        public let bytes: UInt64
        public let selectedCount: UInt64
        public let selectedBytes: UInt64
    }

    public struct Finding: Decodable, Sendable, Equatable {
        public let path: String
        public let rule: String
        public let `class`: RiskClass
        public let bytes: UInt64
        public let selected: Bool
    }

    public enum RiskClass: String, Decodable, Sendable {
        case safe, rebuildable, review
        case userData = "user-data"
    }

    public static func read(_ data: Data) throws -> ScanReport {
        struct Header: Decodable { let schemaVersion: Int }
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let version = try decoder.decode(Header.self, from: data).schemaVersion
        guard version == schemaVersion else { throw ReadError.otherSchema(version) }
        return try decoder.decode(ScanReport.self, from: data)
    }
}
