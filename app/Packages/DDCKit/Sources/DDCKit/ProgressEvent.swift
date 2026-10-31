import Foundation

/// One line of `--progress json` on the CLI's stderr (story "CLI: progress
/// events"; `ProgressLine` in `crates/ddc-core/src/progress.rs`, schema in
/// `docs/progress.schema.json`). Other lines share the stream: `ddc:`
/// warnings and the signal's notice are not events.
public enum ProgressEvent: Sendable, Equatable {
    /// A phase begins.
    case phase(Phase)
    /// A walk's counters.
    case walk(Walk)
    /// The clean's item counters.
    case items(Items)
    /// One item's outcome, after its journal record.
    case item(Item)

    /// `PROGRESS_SCHEMA_VERSION` of the CLI this app embeds.
    public static let schemaVersion = 4
    /// Every `event` value, as the schema lists them.
    public static let names = ["phase", "walk", "items", "item"]

    public enum Phase: String, Decodable, Sendable, CaseIterable {
        /// After the lock, before the clean's re-check walk.
        case preparing
        /// Immediately before the first action: from here on the clean may touch files.
        case cleaning
    }

    public enum WalkStep: String, Decodable, Sendable, CaseIterable {
        /// `ddc scan`.
        case scan
        /// The clean's re-check of the disk before its plan.
        case recheck
        /// The clean's walk of the chosen items for keep markers.
        case markers
        /// `ddc disk`: the whole disk, size only.
        case disk
    }

    public struct Walk: Decodable, Sendable, Equatable {
        public let step: WalkStep
        public let files: UInt64
        public let dirs: UInt64
        public let bytes: UInt64
        /// An estimate; absent for `scan`.
        public let filesExpected: UInt64?
    }

    public struct Items: Decodable, Sendable, Equatable {
        /// Items with an outcome; `not-run` items are not counted.
        public let done: UInt64
        /// Items the executor runs; items refused at the re-check are only in the report.
        public let n: UInt64
        /// `native-only` items are in neither byte figure.
        public let bytesDone: UInt64
        public let bytesTotal: UInt64
        public let current: Current?
    }

    /// The item in flight.
    public struct Current: Decodable, Sendable, Equatable {
        /// 1-based, of `n`.
        public let i: UInt64
        public let path: String
        public let action: ListedAction
        public let bytes: UInt64
        /// The program whose command (or location probe) runs now.
        public let tool: String?
    }

    public struct Item: Decodable, Sendable, Equatable {
        public let i: UInt64
        public let n: UInt64
        public let path: String
        public let action: ListedAction
        public let bytes: UInt64
        public let outcome: CleanReport.Outcome

        private enum CodingKeys: String, CodingKey {
            case i, n, path, action, bytes
        }

        public init(from decoder: any Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            i = try c.decode(UInt64.self, forKey: .i)
            n = try c.decode(UInt64.self, forKey: .n)
            path = try c.decode(String.self, forKey: .path)
            action = try c.decode(ListedAction.self, forKey: .action)
            bytes = try c.decode(UInt64.self, forKey: .bytes)
            outcome = try CleanReport.Outcome(from: decoder)
        }
    }

    /// What a stderr line holds.
    public enum Line: Sendable, Equatable {
        case event(ProgressEvent)
        /// An event of another schema version: ignored, and logged once per run.
        case otherVersion(Int)
    }

    private struct Header: Decodable {
        let schemaVersion: Int
        let event: String
    }

    /// Nil for a line that is not an event: not JSON, not an object of this
    /// schema, or an event this app does not know.
    public static func read(_ line: String) -> Line? {
        guard line.hasPrefix("{") else { return nil }
        let data = Data(line.utf8)
        let decoder = JSONDecoder.snakeCase
        guard let header = try? decoder.decode(Header.self, from: data) else { return nil }
        guard header.schemaVersion == schemaVersion else { return .otherVersion(header.schemaVersion) }
        let event: ProgressEvent? = switch header.event {
        case "phase": (try? decoder.decode(PhaseLine.self, from: data)).map { .phase($0.phase) }
        case "walk": (try? decoder.decode(Walk.self, from: data)).map { .walk($0) }
        case "items": (try? decoder.decode(Items.self, from: data)).map { .items($0) }
        case "item": (try? decoder.decode(Item.self, from: data)).map { .item($0) }
        default: nil
        }
        return event.map { .event($0) }
    }

    private struct PhaseLine: Decodable {
        let phase: Phase
    }
}
