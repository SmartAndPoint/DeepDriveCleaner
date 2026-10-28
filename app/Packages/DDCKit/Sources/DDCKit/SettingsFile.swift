import Foundation

/// `settings.json` as a whole: every key of the CLI's `Settings`
/// (`crates/ddc-core/src/settings.rs`, schema in `docs/settings.schema.json`),
/// so the app can write it back without losing a key it did not edit, the
/// `scheduler` record included (story "Settings: folders, types, notifications").
///
/// It is written as the CLI writes it: snake_case, keys sorted, empty lists
/// and absent values left out. An empty `skips` is never written: a 0.2.x CLI
/// refuses the key.
public struct SettingsFile: Codable, Sendable, Equatable {
    public var schemaVersion = SharedSettings.schemaVersion
    /// Empty means the home folder.
    public var roots: [String] = []
    /// "Count, never clean": read and sized, reported as `user.kept`.
    public var excludes: [String] = []
    /// "Skip entirely": never read, listed in the report's `not_scanned`.
    public var skips: [String] = []
    public var disabledRules: [String] = []
    public var minAgeDays: UInt64?
    public var minSize: UInt64?
    public var notifyThreshold: UInt64?
    public var notifyLowDisk: Bool?
    /// Who runs the scheduled scan (menu bar ADR §5); the app never edits it here.
    public var scheduler: Scheduler?

    public struct Scheduler: Codable, Sendable, Equatable {
        public var by: String
        public var app: String
    }

    public init() {}

    private enum CodingKeys: String, CodingKey {
        case schemaVersion, roots, excludes, skips, disabledRules, minAgeDays, minSize, notifyThreshold, notifyLowDisk, scheduler
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        // serde refuses `null` for a list; so does the app, or it would rewrite a file the CLI calls malformed.
        for key in [CodingKeys.roots, .excludes, .skips, .disabledRules] where c.contains(key) {
            if try c.decodeNil(forKey: key) {
                throw DecodingError.valueNotFound([String].self, .init(codingPath: [key], debugDescription: "\(key.stringValue) is null"))
            }
        }
        schemaVersion = try c.decode(Int.self, forKey: .schemaVersion)
        roots = try c.decodeIfPresent([String].self, forKey: .roots) ?? []
        excludes = try c.decodeIfPresent([String].self, forKey: .excludes) ?? []
        skips = try c.decodeIfPresent([String].self, forKey: .skips) ?? []
        disabledRules = try c.decodeIfPresent([String].self, forKey: .disabledRules) ?? []
        minAgeDays = try c.decodeIfPresent(UInt64.self, forKey: .minAgeDays)
        minSize = try c.decodeIfPresent(UInt64.self, forKey: .minSize)
        notifyThreshold = try c.decodeIfPresent(UInt64.self, forKey: .notifyThreshold)
        notifyLowDisk = try c.decodeIfPresent(Bool.self, forKey: .notifyLowDisk)
        scheduler = try c.decodeIfPresent(Scheduler.self, forKey: .scheduler)
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(schemaVersion, forKey: .schemaVersion)
        if !roots.isEmpty { try c.encode(roots, forKey: .roots) }
        if !excludes.isEmpty { try c.encode(excludes, forKey: .excludes) }
        if !skips.isEmpty { try c.encode(skips, forKey: .skips) }
        if !disabledRules.isEmpty { try c.encode(disabledRules, forKey: .disabledRules) }
        try c.encodeIfPresent(minAgeDays, forKey: .minAgeDays)
        try c.encodeIfPresent(minSize, forKey: .minSize)
        try c.encodeIfPresent(notifyThreshold, forKey: .notifyThreshold)
        try c.encodeIfPresent(notifyLowDisk, forKey: .notifyLowDisk)
        try c.encodeIfPresent(scheduler, forKey: .scheduler)
    }

    /// No file gives the defaults, as for the CLI. The version is read first:
    /// a newer file reads as newer, not as unknown keys (`settings.rs::parse`).
    /// A file the CLI would refuse is refused here too.
    public static func read(at url: URL) throws(SharedSettings.ReadError) -> SettingsFile {
        let data: Data
        do {
            data = try Data(contentsOf: url)
        } catch CocoaError.fileReadNoSuchFile {
            return SettingsFile()
        } catch {
            throw .malformed(path: url.path, reason: error.localizedDescription)
        }
        guard let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else {
            throw .malformed(path: url.path, reason: "not a JSON object")
        }
        guard let version = (object["schema_version"] as? NSNumber)?.intValue else {
            throw .malformed(path: url.path, reason: "no schema_version")
        }
        guard version == SharedSettings.schemaVersion else { throw .otherSchema(version) }
        if let unknown = object.keys.sorted().first(where: { !SharedSettings.keys.contains($0) }) {
            throw .malformed(path: url.path, reason: "unknown key \(unknown)")
        }
        let file: SettingsFile
        do {
            file = try JSONDecoder.snakeCase.decode(SettingsFile.self, from: data)
        } catch {
            throw .malformed(path: url.path, reason: "a field has the wrong type")
        }
        if let relative = (file.roots + file.excludes + file.skips).first(where: { !$0.hasPrefix("/") }) {
            throw .malformed(path: url.path, reason: "\(relative) is not an absolute path")
        }
        if let days = file.minAgeDays, days > SharedSettings.maxMinAgeDays {
            throw .malformed(path: url.path, reason: "min_age_days is over \(SharedSettings.maxMinAgeDays)")
        }
        return file
    }

    /// The bytes the app writes: pretty, keys sorted, a newline at the end.
    public func encoded() throws -> Data {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        return try encoder.encode(self) + Data("\n".utf8)
    }
}
