import Foundation

/// The report of `deepdrivecleaner keep remove --json` (the contract is
/// `crates/ddc-core/src/keep.rs`, schema in `docs/keep.schema.json`; story
/// "Protected by you: say what it is, and Remove Protection"). Strict like
/// `FreeReport`: another schema version is unreadable, and an unknown result
/// or reason is a decoding error.
public struct KeepReport: Decodable, Sendable, Equatable {
    /// `KEEP_SCHEMA_VERSION` of the CLI this app embeds.
    public static let schemaVersion = 1

    public let schemaVersion: Int
    /// The folder that held the marker, as it was given.
    public let path: String
    /// The folder's identity as given, else as the open found it.
    public let dev: UInt64?
    public let ino: UInt64?
    /// The rule the marker kept, from the saved scan report.
    public let rule: String?
    public let result: Outcome

    /// What became of the marker, tagged by `result`.
    public enum Outcome: Sendable, Equatable {
        /// The `.ddc-keep` was unlinked.
        case removed
        /// The folder is there with no `.ddc-keep` in it.
        case absent
        /// A check refused it: nothing was removed.
        case refused(Refusal)
        /// The open, the stat or the unlink failed.
        case failed(error: String)

        /// Every `result` value, as the schema lists them.
        public static let tags = ["removed", "absent", "refused", "failed"]

        /// The protection is gone, now or before: the app scans again.
        public var unprotected: Bool { self == .removed || self == .absent }
    }

    /// The CLI's `KeepRefusal`.
    public enum Refusal: String, Decodable, Sendable, CaseIterable {
        case outsideScanRoots = "outside-scan-roots"
        case changedSinceScan = "changed-since-scan"
        case noFolder = "no-folder"
        /// A symlink or a folder named `.ddc-keep`: removed in Finder.
        case notAFile = "not-a-file"
    }

    private enum CodingKeys: String, CodingKey {
        case schemaVersion, path, dev, ino, rule, result, reason, error
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try c.decode(Int.self, forKey: .schemaVersion)
        path = try c.decode(String.self, forKey: .path)
        dev = try c.decodeIfPresent(UInt64.self, forKey: .dev)
        ino = try c.decodeIfPresent(UInt64.self, forKey: .ino)
        rule = try c.decodeIfPresent(String.self, forKey: .rule)
        switch try c.decode(String.self, forKey: .result) {
        case "removed": result = .removed
        case "absent": result = .absent
        case "refused": result = .refused(try c.decode(Refusal.self, forKey: .reason))
        case "failed": result = .failed(error: try c.decode(String.self, forKey: .error))
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .result, in: c, debugDescription: "unknown result \(other)")
        }
    }

    public static func read(_ data: Data) throws -> KeepReport {
        let decoder = JSONDecoder.snakeCase
        let version = try decoder.decode(VersionHeader.self, from: data).schemaVersion
        guard version == schemaVersion else { throw ScanReport.ReadError.otherSchema(version) }
        return try decoder.decode(KeepReport.self, from: data)
    }
}
