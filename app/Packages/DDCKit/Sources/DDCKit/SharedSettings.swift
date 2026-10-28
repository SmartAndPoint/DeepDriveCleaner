import Foundation

/// The part of `settings.json` the popover reads (the contract is
/// `crates/ddc-core/src/settings.rs`, schema in `docs/settings.schema.json`):
/// what makes a finding `ignored`, and the notification threshold.
///
/// The app shares this file with the CLI (ADR §6); it reads it here, and
/// `SettingsStore` writes it. App-only preferences never go in it: the CLI
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
        "schema_version", "roots", "excludes", "skips", "disabled_rules", "min_age_days", "min_size",
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

    /// Decoded through `SettingsFile`, with its strict checks: a file the
    /// app would refuse to write over is one the popover never applies.
    public static func read(at url: URL) throws(ReadError) -> SharedSettings {
        SharedSettings(file: try SettingsFile.read(at: url))
    }

    /// What the popover reads of a whole file.
    public init(file: SettingsFile) {
        roots = file.roots
        disabledRules = file.disabledRules
        minSize = file.minSize ?? 0
        notifyThreshold = file.notifyThreshold
    }
}
