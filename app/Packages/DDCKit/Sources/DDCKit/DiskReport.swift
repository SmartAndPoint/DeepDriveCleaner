import Foundation

/// The report of `deepdrivecleaner disk --json`, also saved as
/// `last-disk.json` (the contract is `crates/ddc-core/src/disk/model.rs`,
/// schema in `docs/disk.schema.json`; story "Whole disk in the app").
///
/// As with `ScanReport`, the schema version is read first: a report of
/// another version is unreadable as a whole, never half-decoded. Every enum
/// is strict: an unknown value is a decoding error, except `AreaReveal`: an
/// unknown one is no button.
public struct DiskReport: Decodable, Sendable, Equatable {
    /// `DISK_SCHEMA_VERSION` of the CLI this app embeds.
    public static let schemaVersion = 1

    public let schemaVersion: Int
    public let ddcVersion: String
    /// When the measurement started (RFC 3339).
    public let generatedAt: String
    public let durationMs: UInt64
    /// Stopped by a signal: the areas not finished are `partial`.
    public let cancelled: Bool
    /// The startup container; nil when neither `diskutil` nor `statfs` gave it.
    public let container: Container?
    public let home: HomeRow
    /// In the CLI's order; a nested area names its `parent` and follows it.
    public let areas: [Area]
    public let simulators: Simulators?
    public let snapshots: Snapshots
    public let walk: WalkTotals
    public let notCounted: NotCounted

    public static func read(_ data: Data) throws -> DiskReport {
        let decoder = JSONDecoder.snakeCase
        let version = try decoder.decode(VersionHeader.self, from: data).schemaVersion
        guard version == schemaVersion else { throw ScanReport.ReadError.otherSchema(version) }
        return try decoder.decode(DiskReport.self, from: data)
    }

    public struct WalkTotals: Decodable, Sendable, Equatable {
        public let files: UInt64
        public let dirs: UInt64
        public let bytes: UInt64
    }

    public enum ContainerSource: String, Decodable, Sendable, CaseIterable {
        case diskutil, statfs
    }

    public struct Container: Decodable, Sendable, Equatable {
        public let source: ContainerSource
        public let reference: String?
        public let capacityBytes: UInt64
        public let inUseBytes: UInt64
        public let volumes: [Volume]
    }

    public struct Volume: Decodable, Sendable, Equatable {
        public let name: String
        public let role: String
        public let device: String?
        public let usedBytes: UInt64?
        public let mountPoint: String?
    }

    public enum HomeSource: String, Decodable, Sendable, CaseIterable {
        case lastScan = "last-scan"
        case walk
    }

    public struct HomeRow: Decodable, Sendable, Equatable {
        public let path: String
        public let source: HomeSource
        /// The scan's `generated_at`, or this measurement's.
        public let asOf: String
        public let bytes: UInt64
        public let reclaimableBytes: UInt64?
        public let unreadable: UInt64
        public let partial: Bool
    }

    /// What frees an area with its owning tool (story "Whole disk: free
    /// space with the owning tool").
    public enum ActionId: String, Decodable, Sendable, CaseIterable {
        case simctlRuntimeDelete = "simctl-runtime-delete"
        case brewCleanup = "brew-cleanup"
    }

    public struct Area: Decodable, Sendable, Equatable, Identifiable {
        public let id: String
        public let paths: [String]
        public let parent: String?
        public let title: String
        public let explanation: String
        public let frees: String
        public let action: ActionId?
        /// Show in Finder or Open Software Update; nil for a row with
        /// nothing to press, and for a value this app does not know.
        public let reveal: AreaReveal?
        public let bytes: UInt64
        public let sharedBytes: UInt64
        public let files: UInt64
        public let dirs: UInt64
        public let unreadable: UInt64
        /// Not measured to the end: its size is a lower bound.
        public let partial: Bool
        public let entriesRead: UInt64?
        /// `ddc disk free` ran on it after this report was measured: its
        /// size is from before. The CLI leaves it out when false.
        public let stale: Bool?

        public var measuredBeforeAction: Bool { stale == true }

        private enum CodingKeys: String, CodingKey {
            case id, paths, parent, title, explanation, frees, action, reveal, bytes, sharedBytes, files, dirs
            case unreadable, partial, entriesRead, stale
        }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            id = try c.decode(String.self, forKey: .id)
            paths = try c.decode([String].self, forKey: .paths)
            parent = try c.decodeIfPresent(String.self, forKey: .parent)
            title = try c.decode(String.self, forKey: .title)
            explanation = try c.decode(String.self, forKey: .explanation)
            frees = try c.decode(String.self, forKey: .frees)
            action = try c.decodeIfPresent(ActionId.self, forKey: .action)
            reveal = try c.decodeIfPresent(String.self, forKey: .reveal).flatMap(AreaReveal.init(rawValue:))
            bytes = try c.decode(UInt64.self, forKey: .bytes)
            sharedBytes = try c.decode(UInt64.self, forKey: .sharedBytes)
            files = try c.decode(UInt64.self, forKey: .files)
            dirs = try c.decode(UInt64.self, forKey: .dirs)
            unreadable = try c.decode(UInt64.self, forKey: .unreadable)
            partial = try c.decode(Bool.self, forKey: .partial)
            entriesRead = try c.decodeIfPresent(UInt64.self, forKey: .entriesRead)
            stale = try c.decodeIfPresent(Bool.self, forKey: .stale)
        }
    }

    /// Where the app sends the user for an area it does not free itself
    /// (story "Whole disk: Show in Finder and the honest action for each
    /// row"). Not `Decodable`: `Area` reads it leniently.
    public enum AreaReveal: String, Sendable, CaseIterable {
        case finder
        case softwareUpdate = "software-update"
    }

    public enum SimulatorStatus: String, Decodable, Sendable, CaseIterable {
        case listed
        case firstLaunchPending = "first-launch-pending"
        case failed
    }

    public struct Simulators: Decodable, Sendable, Equatable {
        public let status: SimulatorStatus
        public let message: String?
        public let runtimes: [Runtime]
        public let listedBytes: UInt64
    }

    public struct Runtime: Decodable, Sendable, Equatable {
        public let identifier: String
        public let runtimeIdentifier: String?
        public let platform: String?
        public let version: String?
        public let build: String?
        public let sizeBytes: UInt64?
        public let path: String?
        public let parentMountPath: String?
        public let deletable: Bool?
        public let state: String?
        public let devices: [Device]
        public let booted: Bool
    }

    public struct Device: Decodable, Sendable, Equatable {
        public let udid: String
        public let name: String
        public let state: String
    }

    public struct Snapshots: Decodable, Sendable, Equatable {
        /// Nil when `tmutil` could not count them.
        public let count: UInt64?
    }

    public enum InconsistentCause: String, Decodable, Sendable, CaseIterable {
        case staleHomeRow = "stale-home-row"
        case clonesCountedTwice = "clones-counted-twice"
    }

    public enum HeldKind: String, Decodable, Sendable, CaseIterable {
        case cloud, skip
    }

    public struct Held: Decodable, Sendable, Equatable {
        public let kind: HeldKind
        public let path: String
    }

    public struct NotCounted: Decodable, Sendable, Equatable {
        /// The Data volume's use less what was counted; nil without the
        /// volume's size, negative when the rows overlap.
        public let bytes: Int64?
        public let dataVolumeBytes: UInt64?
        public let countedBytes: UInt64
        public let inconsistent: Bool
        public let causes: [InconsistentCause]
        public let holds: [Held]
        public let unreadable: UInt64

        private enum CodingKeys: String, CodingKey {
            case bytes, dataVolumeBytes, countedBytes, inconsistent, causes, holds, unreadable
        }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            bytes = try c.decodeIfPresent(Int64.self, forKey: .bytes)
            dataVolumeBytes = try c.decodeIfPresent(UInt64.self, forKey: .dataVolumeBytes)
            countedBytes = try c.decode(UInt64.self, forKey: .countedBytes)
            inconsistent = try c.decode(Bool.self, forKey: .inconsistent)
            causes = try c.decodeIfPresent([InconsistentCause].self, forKey: .causes) ?? []
            holds = try c.decode([Held].self, forKey: .holds)
            unreadable = try c.decode(UInt64.self, forKey: .unreadable)
        }
    }
}
