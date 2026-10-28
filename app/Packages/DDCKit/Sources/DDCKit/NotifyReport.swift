import Foundation

/// What `deepdrivecleaner scan --background --notify json` prints: the
/// background scan's notify decision (the contract is
/// `crates/ddc-core/src/notify.rs`, schema in `docs/notify.schema.json`).
/// The rule stays in the CLI; the app shows the decision and records it with
/// `notify ack` (menu bar ADR §5).
public struct NotifyReport: Sendable, Equatable {
    /// `NOTIFY_SCHEMA_VERSION` of the CLI this app embeds.
    public static let schemaVersion = 1

    public enum ReadError: Error, Equatable {
        case otherSchema(Int)
        case unknownStatus(String)
    }

    public enum Status: Sendable, Equatable {
        /// The scan ran and `last-scan.json` holds its report.
        case scanned(NotifyDecision)
        /// Nothing was scanned: a clean held the lock (`busy`).
        case skipped(reason: String)
    }

    public let status: Status

    private struct Header: Decodable {
        let schemaVersion: Int
        let status: String
        let reason: String?
    }

    public static func read(_ data: Data) throws -> NotifyReport {
        let decoder = JSONDecoder.snakeCase
        let header = try decoder.decode(Header.self, from: data)
        guard header.schemaVersion == schemaVersion else { throw ReadError.otherSchema(header.schemaVersion) }
        switch header.status {
        case "scanned": return NotifyReport(status: .scanned(try decoder.decode(NotifyDecision.self, from: data)))
        case "skipped": return NotifyReport(status: .skipped(reason: header.reason ?? ""))
        default: throw ReadError.unknownStatus(header.status)
        }
    }
}

/// The CLI's `Decision`: whether to notify, why, and the amounts. Codable,
/// so a decision held back by working hours keeps in `UserDefaults`.
public struct NotifyDecision: Codable, Sendable, Equatable {
    public enum Reason: String, Codable, Sendable, CaseIterable {
        case threshold
        case lowDisk = "low-disk"
    }

    public var notify: Bool
    /// Nil exactly when `notify` is false; low disk wins when both apply.
    public var reason: Reason?
    /// The volume's mount point.
    public var volume: String?
    /// What the default selection frees on all volumes together.
    public var reclaimable: UInt64
    public var free: UInt64?
    public var total: UInt64?

    public init(notify: Bool, reason: Reason?, volume: String? = nil, reclaimable: UInt64, free: UInt64? = nil, total: UInt64? = nil) {
        self.notify = notify
        self.reason = reason
        self.volume = volume
        self.reclaimable = reclaimable
        self.free = free
        self.total = total
    }
}
