import Foundation

/// `deepdrivecleaner rules --json`: every rule with its group (the contract is
/// `crates/ddc-core/src/rules/listing.rs`, schema in `docs/rules.schema.json`).
/// The groups come from the CLI, so a new rule lands in the right group
/// without an app release.
public struct RuleList: Decodable, Sendable, Equatable {
    /// `RULES_SCHEMA_VERSION` of the CLI this app embeds.
    public static let schemaVersion = 1

    public enum ReadError: Error, Equatable {
        case otherSchema(Int)
    }

    public let schemaVersion: Int
    /// Group labels in display order.
    public let groups: [String]
    public let rules: [Rule]

    public struct Rule: Decodable, Sendable, Equatable {
        public let id: String
        public let group: String
        public let `class`: ScanReport.RiskClass
        /// The Types settings may switch it off.
        public let switchable: Bool
    }

    public func group(of rule: String) -> String? {
        rules.first { $0.id == rule }?.group
    }

    public static func read(_ data: Data) throws -> RuleList {
        let decoder = JSONDecoder.snakeCase
        let version = try decoder.decode(VersionHeader.self, from: data).schemaVersion
        guard version == schemaVersion else { throw ReadError.otherSchema(version) }
        return try decoder.decode(RuleList.self, from: data)
    }
}
