import Foundation

/// The part of `settings.json` the popover reads (the contract is
/// `crates/ddc-core/src/settings.rs`, schema in `docs/settings.schema.json`):
/// what makes a finding `ignored`, and the notification threshold.
///
/// The app shares this file with the CLI (ADR §6); it reads it here and the
/// Settings story writes it. App-only preferences never go in it: the CLI
/// refuses unknown keys. A file the CLI would refuse is refused here too, so
/// the popover never applies settings no scan would use.
public struct SharedSettings: Sendable, Equatable {
    /// `SETTINGS_SCHEMA_VERSION` of the CLI this app embeds.
    public static let schemaVersion = 1
    /// `DEFAULT_THRESHOLD` of the CLI's background scan.
    public static let defaultThreshold: UInt64 = 10 << 30
    /// `MAX_MIN_AGE_DAYS` of the CLI.
    static let maxMinAgeDays: UInt64 = 3650
    /// Every key of the CLI's `Settings`; it denies any other.
    static let keys: Set<String> = [
        "schema_version", "roots", "excludes", "disabled_rules", "min_age_days", "min_size",
        "notify_threshold", "notify_low_disk", "scheduler",
    ]

    public enum ReadError: Error, Equatable {
        case malformed(path: String, reason: String)
        case otherSchema(Int)
    }

    public var roots: [String] = []
    public var disabledRules: [String] = []
    public var minSize: UInt64 = 0
    public var notifyThreshold: UInt64?

    public init() {}

    public var threshold: UInt64 { notifyThreshold ?? Self.defaultThreshold }

    /// Every field with its type, so a wrong type is refused as the CLI refuses it.
    private struct File: Decodable {
        let roots: [String]?
        let excludes: [String]?
        let disabledRules: [String]?
        let minAgeDays: UInt64?
        let minSize: UInt64?
        let notifyThreshold: UInt64?
        let notifyLowDisk: Bool?
        let scheduler: Scheduler?
    }

    private struct Scheduler: Decodable {
        let by: String
        let app: String
    }

    /// No file gives the defaults, as for the CLI. The version is read first:
    /// a newer file reads as newer, not as unknown keys (`settings.rs::parse`).
    public static func read(at url: URL) throws(ReadError) -> SharedSettings {
        let data: Data
        do {
            data = try Data(contentsOf: url)
        } catch CocoaError.fileReadNoSuchFile {
            return SharedSettings()
        } catch {
            throw .malformed(path: url.path, reason: error.localizedDescription)
        }
        guard let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else {
            throw .malformed(path: url.path, reason: "not a JSON object")
        }
        guard let version = (object["schema_version"] as? NSNumber)?.intValue else {
            throw .malformed(path: url.path, reason: "no schema_version")
        }
        guard version == schemaVersion else { throw .otherSchema(version) }
        if let unknown = object.keys.sorted().first(where: { !keys.contains($0) }) {
            throw .malformed(path: url.path, reason: "unknown key \(unknown)")
        }
        let file: File
        do {
            file = try JSONDecoder.snakeCase.decode(File.self, from: data)
        } catch {
            throw .malformed(path: url.path, reason: "a field has the wrong type")
        }
        if let relative = (file.roots ?? []).first(where: { !$0.hasPrefix("/") }) ?? (file.excludes ?? []).first(where: { !$0.hasPrefix("/") }) {
            throw .malformed(path: url.path, reason: "\(relative) is not an absolute path")
        }
        if let days = file.minAgeDays, days > maxMinAgeDays {
            throw .malformed(path: url.path, reason: "min_age_days is over \(maxMinAgeDays)")
        }
        var settings = SharedSettings()
        settings.roots = file.roots ?? []
        settings.disabledRules = file.disabledRules ?? []
        settings.minSize = file.minSize ?? 0
        settings.notifyThreshold = file.notifyThreshold
        return settings
    }
}
