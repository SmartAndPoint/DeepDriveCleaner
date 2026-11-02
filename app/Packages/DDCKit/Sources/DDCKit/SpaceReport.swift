import Foundation

/// What `deepdrivecleaner space --json` prints (the contract is
/// `crates/ddc-core/src/space.rs`, schema in `docs/space.schema.json`; story
/// "Popover: say when used space grew since the last scan", menu bar ADR §6
/// item 13). The rule stays in the CLI: the app reads `state` and `grew` and
/// compares nothing itself. Strict like `KeepReport`: another schema version
/// is unreadable, and an unknown state or source is a decoding error; keys it
/// does not know are ignored.
public struct SpaceReport: Decodable, Sendable, Equatable {
    /// `SPACE_SCHEMA_VERSION` of the CLI this app embeds.
    public static let schemaVersion = 1

    public enum State: String, Decodable, Sendable, CaseIterable {
        case quiet
        /// In-use space grew by at least the CLI's threshold since the baseline.
        case grew
        /// Under 10 % free; it wins over `grew`.
        case lowDisk = "low-disk"
    }

    /// The home folder's volume, read now.
    public struct Volume: Decodable, Sendable, Equatable {
        public let mountPoint: String
        public let totalBytes: UInt64
        public let freeBytes: UInt64
    }

    /// What the saved report recorded for that volume.
    public struct Baseline: Decodable, Sendable, Equatable {
        /// Named by what happened, so no `.clean` reads as a clean run here
        /// (`ContractTests.cleanHasOneOwner`); the raw values are the CLI's.
        public enum Source: String, Codable, Sendable, CaseIterable {
            /// The scan read the free space, at the report's `generated_at`.
            case scanned = "scan"
            /// A `clean --items` edit read it again, at the volume's `read_at`.
            case cleaned = "clean"
        }

        public let source: Source
        /// RFC 3339, as the saved report writes it.
        public let at: String
        public let freeBytes: UInt64
    }

    public let schemaVersion: Int
    public let checkedAt: String
    public let volume: Volume
    /// Null with no saved report, or none that lists this volume.
    public let baseline: Baseline?
    /// In-use bytes now minus at the baseline; null exactly when `baseline` is.
    public let inUseDelta: Int64?
    /// In-use space reached the threshold, also when `low-disk` wins.
    public let grew: Bool
    public let state: State

    private enum CodingKeys: String, CodingKey {
        case schemaVersion, checkedAt, volume, baseline, inUseDelta, grew, state
    }

    /// `baseline` and `in_use_delta` are always written, null when absent:
    /// required here too, so a CLI that drops them is refused, not read as "no baseline".
    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schemaVersion = try c.decode(Int.self, forKey: .schemaVersion)
        checkedAt = try c.decode(String.self, forKey: .checkedAt)
        volume = try c.decode(Volume.self, forKey: .volume)
        baseline = try c.decode(Baseline?.self, forKey: .baseline)
        inUseDelta = try c.decode(Int64?.self, forKey: .inUseDelta)
        grew = try c.decode(Bool.self, forKey: .grew)
        state = try c.decode(State.self, forKey: .state)
    }

    public static func read(_ data: Data) throws -> SpaceReport {
        let decoder = JSONDecoder.snakeCase
        let version = try decoder.decode(VersionHeader.self, from: data).schemaVersion
        guard version == schemaVersion else { throw ScanReport.ReadError.otherSchema(version) }
        return try decoder.decode(SpaceReport.self, from: data)
    }
}

/// The growth badge's "once per baseline" (menu bar ADR §5, "A second
/// source"): presentation, so it lives in the app, never in the CLI, and a
/// `ddc space` run from a terminal cannot use it up.
public enum GrowthBadge {
    /// A baseline: the volume, and which reading of its free space the
    /// comparison is against. `source` and `at` are nil for "no saved report
    /// with that volume", so a low disk with no report badges once until one appears.
    public struct Key: Codable, Sendable, Equatable {
        public let mountPoint: String
        public let source: SpaceReport.Baseline.Source?
        public let at: String?

        public init(mountPoint: String, source: SpaceReport.Baseline.Source?, at: String?) {
            self.mountPoint = mountPoint
            self.source = source
            self.at = at
        }

        /// The baseline a `space` result compared with.
        public init(_ space: SpaceReport) {
            self.init(mountPoint: space.volume.mountPoint, source: space.baseline?.source, at: space.baseline?.at)
        }

        /// The baseline a saved report gives that volume, as the CLI picks
        /// it: the volume's `read_at` when a clean read it again, else the
        /// report's `generated_at`; none without the report or the volume.
        public init(report: ScanReport?, mountPoint: String) {
            guard let report, let volume = report.volumes.first(where: { $0.mountPoint == mountPoint }) else {
                self.init(mountPoint: mountPoint, source: nil, at: nil)
                return
            }
            if let readAt = volume.readAt {
                self.init(mountPoint: mountPoint, source: .cleaned, at: readAt)
            } else {
                self.init(mountPoint: mountPoint, source: .scanned, at: report.generatedAt)
            }
        }
    }

    /// The baseline to set the badge for, or nil: the first `grew` or
    /// `low-disk` for a baseline sets it, once.
    public static func next(_ space: SpaceReport, badgedFor: Key?) -> Key? {
        guard space.state != .quiet else { return nil }
        let key = Key(space)
        return key == badgedFor ? nil : key
    }
}

/// The popover's band between the header and the main block: that used
/// space grew, by how much and since when, or that the disk is nearly full.
/// A pure value, so every state is a unit test without SwiftUI.
public struct GrowthBand: Sendable, Equatable {
    public enum ScanButton: Sendable, Equatable {
        case none, enabled, disabled
    }

    /// "↑ 198.0 GiB more in use on the startup disk than at the last scan, 26 h ago."
    public let text: String
    /// "A scan can tell where.", or the growth clause under a low-disk line,
    /// with "A scan can tell where." after it unless scans are paused.
    public let detail: String?
    /// Low disk: drawn in the warning ink.
    public let warning: Bool
    public let scanButton: ScanButton

    public init(text: String, detail: String?, warning: Bool, scanButton: ScanButton) {
        self.text = text
        self.detail = detail
        self.warning = warning
        self.scanButton = scanButton
    }

    /// What VoiceOver reads for `text` and `detail`: the arrow is drawn, not said.
    public var spokenText: String { Self.spoken(text) }
    public var spokenDetail: String? { detail.map(Self.spoken) }

    static func spoken(_ line: String) -> String {
        line.hasPrefix("↑ ") ? String(line.dropFirst(2)) : line
    }

    /// Nil for `quiet`. While scans are paused a growth band gives the
    /// number only: no "A scan can tell where." and no Scan Now. A low-disk
    /// band offers the scan only when the CLI says used space grew too.
    public static func make(_ space: SpaceReport, now: Date, paused: Bool, canScan: Bool) -> GrowthBand? {
        switch space.state {
        case .quiet:
            return nil
        case .grew:
            guard let clause = growth(space, now: now, naming: true) else { return nil }
            if paused { return GrowthBand(text: clause, detail: nil, warning: false, scanButton: .none) }
            return GrowthBand(text: clause, detail: "A scan can tell where.", warning: false, scanButton: canScan ? .enabled : .disabled)
        case .lowDisk:
            let disk = Wording.volume(space.volume.mountPoint)
            let text = Wording.percentFull(free: space.volume.freeBytes, total: space.volume.totalBytes)
                .map { "\(disk) is \($0)% full." } ?? "\(disk) is almost full."
            // The growth clause only when the CLI says so: no comparison here.
            guard space.grew, let clause = growth(space, now: now, naming: false) else {
                return GrowthBand(text: text, detail: nil, warning: true, scanButton: .none)
            }
            if paused { return GrowthBand(text: text, detail: clause, warning: true, scanButton: .none) }
            return GrowthBand(text: text, detail: "\(clause) A scan can tell where.", warning: true, scanButton: canScan ? .enabled : .disabled)
        }
    }

    /// "↑ 198.0 GiB more in use on the startup disk than at the last scan,
    /// 26 h ago." or "… than after your clean, 2 h ago."; without the volume
    /// under a low-disk line, which names it.
    private static func growth(_ space: SpaceReport, now: Date, naming: Bool) -> String? {
        guard let baseline = space.baseline, let delta = space.inUseDelta, delta > 0 else { return nil }
        let since = baseline.source == .cleaned ? "after your clean" : "at the last scan"
        let on = naming ? " on \(Wording.volumeMidSentence(space.volume.mountPoint))" : ""
        let age = Wording.date(rfc3339: baseline.at).map { ", \(Wording.age(of: $0, now: now))" } ?? ""
        return "↑ \(ByteSize.human(UInt64(delta))) more in use\(on) than \(since)\(age)."
    }
}
