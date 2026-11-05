import Foundation

/// What `deepdrivecleaner changes --json` prints (the contract is
/// `crates/ddc-core/src/changes/`, schema in `docs/changes.schema.json`; menu
/// bar ADR §6 item 15; story "Popover and Review: what grew since the earlier
/// scan"). The rules stay in the CLI: the app reads `grew` and the numbers,
/// and compares no threshold itself. Strict like `SpaceReport`: another
/// schema version is unreadable, and an unknown value of an enum it reads is
/// a decoding error; keys it does not know are ignored.
public struct ChangesReport: Decodable, Sendable, Equatable {
    /// `CHANGES_SCHEMA_VERSION` of the CLI this app embeds.
    public static let schemaVersion = 1

    public enum Status: String, Decodable, Sendable, CaseIterable {
        case compared
        case notCompared = "not-compared"
    }

    public enum Reason: String, Decodable, Sendable, CaseIterable {
        case noReport = "no-report"
        case noPrevious = "no-previous"
        case otherSchema = "other-schema"
        case predatesFields = "predates-fields"
        case cancelled
        case noSharedRoot = "no-shared-root"
        case outOfStep = "out-of-step"
    }

    /// One report, as the result names it.
    public struct Side: Decodable, Sendable, Equatable {
        public let writtenAt: String?
        /// The writer, as the CLI names it (`scan`, `clean-items`, …): a plain
        /// string, so no `.clean` case reads as a clean run here
        /// (`ContractTests.cleanHasOneOwner`).
        public let writtenBy: String?
        public let generatedAt: String

        /// Written by a clean's edit of the saved report.
        public var byClean: Bool { writtenBy == "clean" || writtenBy == "clean-items" }
    }

    public struct Disk: Decodable, Sendable, Equatable {
        public let mountPoint: String
        /// In-use bytes now minus at the baseline: positive when it grew.
        public let delta: Int64
    }

    public enum HomeReason: String, Decodable, Sendable, CaseIterable {
        case differentHome = "different-home"
        case differentRoots = "different-roots"
        case differentAccess = "different-access"
        case cutList = "cut-list"
        case differentNotRead = "different-not-read"
    }

    public struct Home: Decodable, Sendable, Equatable {
        public let compared: Bool
        public let reasons: [HomeReason]
        /// Null when the home did not compare.
        public let delta: Int64?
        /// Growth in folders that are no finding; null when the home did not compare.
        public let elsewhere: Int64?

        private enum CodingKeys: String, CodingKey { case compared, reasons, delta, elsewhere }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            compared = try c.decode(Bool.self, forKey: .compared)
            reasons = try c.decode([HomeReason].self, forKey: .reasons)
            delta = try c.decodeIfPresent(Int64.self, forKey: .delta)
            elsewhere = try c.decodeIfPresent(Int64.self, forKey: .elsewhere)
        }
    }

    public enum NotReadKind: String, Decodable, Sendable, CaseIterable {
        case cloud, skip, prompting, refused, evicted
        case otherDisk = "other-disk"
        case snapshot, changed
    }

    public struct NotRead: Decodable, Sendable, Equatable {
        public let path: String
        public let kind: NotReadKind
    }

    public struct Rest: Decodable, Sendable, Equatable {
        /// What the disk's change leaves after the home; null without `disk`.
        public let delta: Int64?
        public let notRead: [NotRead]

        private enum CodingKeys: String, CodingKey { case delta, notRead }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            delta = try c.decodeIfPresent(Int64.self, forKey: .delta)
            notRead = try c.decode([NotRead].self, forKey: .notRead)
        }
    }

    public struct Cleaned: Decodable, Sendable, Equatable {
        public let deleted: UInt64
        public let trashed: UInt64

        public var total: UInt64 { deleted.addingReportingOverflow(trashed).partialValue }
    }

    /// `OriginHarness` of the core: a coding agent, or `git` for a worktree
    /// made by hand.
    public enum Harness: String, Decodable, Sendable, CaseIterable {
        case claudeCode = "claude-code"
        case codex, opencode, cursor, git
    }

    public struct HarnessDelta: Decodable, Sendable, Equatable {
        public let harness: Harness
        public let delta: Int64
        public let count: UInt64
    }

    public struct Group: Decodable, Sendable, Equatable {
        /// The Review group's label, as `rules --json` gives it.
        public let group: String
        /// Net: the person's cleans and the Trash's intake from them left out.
        public let delta: Int64
        public let new: UInt64
        public let grew: UInt64
        public let moved: UInt64
        public let byHarness: [HarnessDelta]
    }

    public enum ItemKind: String, Decodable, Sendable, CaseIterable {
        case new, grew, shrank, gone, cleaned, moved
        case notCompared = "not-compared"
    }

    public struct Item: Decodable, Sendable, Equatable {
        public let path: String
        public let rule: String
        public let kind: ItemKind
        public let before: UInt64?
        public let now: UInt64?
        public let delta: Int64
        public let cleaned: UInt64
        public let ignored: Bool
        /// Shared bytes on either side: the size is an upper bound.
        public let approximate: Bool

        private enum CodingKeys: String, CodingKey { case path, rule, kind, before, now, delta, cleaned, ignored, approximate }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            path = try c.decode(String.self, forKey: .path)
            rule = try c.decode(String.self, forKey: .rule)
            kind = try c.decode(ItemKind.self, forKey: .kind)
            before = try c.decodeIfPresent(UInt64.self, forKey: .before)
            now = try c.decodeIfPresent(UInt64.self, forKey: .now)
            delta = try c.decode(Int64.self, forKey: .delta)
            cleaned = try c.decode(UInt64.self, forKey: .cleaned)
            ignored = try c.decode(Bool.self, forKey: .ignored)
            approximate = try c.decodeIfPresent(Bool.self, forKey: .approximate) ?? false
        }
    }

    public let schemaVersion: Int
    public let status: Status
    public let reason: Reason?
    public let baseline: Side?
    public let current: Side?
    public let comparedRoots: [String]
    public let disk: Disk?
    public let home: Home?
    public let rest: Rest?
    public let cleanedSince: Cleaned?
    public let groups: [Group]
    public let items: [Item]
    /// In-use space grew by at least the CLI's threshold between the two
    /// reports (`ddc space`'s rule).
    public let grew: Bool

    private enum CodingKeys: String, CodingKey {
        case schemaVersion, status, reason, baseline, current, comparedRoots, disk, home, rest, cleanedSince, groups, items, grew
    }

    /// The top-level optional fields are always written, null when absent,
    /// and `grew` always: required here, so a CLI that drops them is refused.
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try c.decode(Int.self, forKey: .schemaVersion)
        status = try c.decode(Status.self, forKey: .status)
        reason = try c.decode(Reason?.self, forKey: .reason)
        baseline = try c.decode(Side?.self, forKey: .baseline)
        current = try c.decode(Side?.self, forKey: .current)
        comparedRoots = try c.decode([String].self, forKey: .comparedRoots)
        disk = try c.decode(Disk?.self, forKey: .disk)
        home = try c.decode(Home?.self, forKey: .home)
        rest = try c.decode(Rest?.self, forKey: .rest)
        cleanedSince = try c.decode(Cleaned?.self, forKey: .cleanedSince)
        groups = try c.decode([Group].self, forKey: .groups)
        items = try c.decode([Item].self, forKey: .items)
        grew = try c.decode(Bool.self, forKey: .grew)
    }

    public static func read(_ data: Data) throws -> ChangesReport {
        let decoder = JSONDecoder.snakeCase
        let version = try decoder.decode(VersionHeader.self, from: data).schemaVersion
        guard version == schemaVersion else { throw ScanReport.ReadError.otherSchema(version) }
        return try decoder.decode(ChangesReport.self, from: data)
    }
}
