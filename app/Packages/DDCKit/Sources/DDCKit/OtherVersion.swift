import Foundation

/// A saved report of another schema than the app reads (story "Popover:
/// after an update, say why a new scan is needed"; menu bar ADR §1, amended
/// 2026-11-08): who made it, so the app can say why a new scan is needed
/// instead of acting as if there had never been one. Readability comes from
/// the schema; the words come from the maker's version against the app's.
public struct OtherVersion: Sendable, Equatable {
    /// The report's `schema_version`.
    public let schema: Int
    /// The schema this app reads.
    public let appSchema: Int
    /// The report's `ddc_version`, when the header gives one.
    public let ddcVersion: String?
    /// The app's own version (its CLI's); nil in a test or a build without one.
    public let appVersion: String?

    public init(schema: Int, appSchema: Int, ddcVersion: String?, appVersion: String?) {
        self.schema = schema
        self.appSchema = appSchema
        self.ddcVersion = ddcVersion
        self.appVersion = appVersion
    }

    public enum Maker: Sendable, Equatable {
        /// An older ddc: the app was updated since.
        case olderThanApp
        /// A newer ddc than the app (a Homebrew CLI, say).
        case newerThanApp
        /// The same version, or the app's own is not known (a development build).
        case same
        /// The header names no version.
        case unknown
    }

    public var maker: Maker {
        guard let ddcVersion else { return .unknown }
        guard let appVersion else { return .same }
        return switch ddcVersion.compare(appVersion, options: .numeric) {
        case .orderedAscending: .olderThanApp
        case .orderedDescending: .newerThanApp
        case .orderedSame: .same
        }
    }

    /// The report is of a newer schema than the app reads.
    public var reportIsNewer: Bool { schema > appSchema }

    /// Updating the app is what reads it, not a new scan.
    public var updateTheApp: Bool {
        switch maker {
        case .olderThanApp: false
        case .newerThanApp: true
        case .same, .unknown: reportIsNewer
        }
    }
}

extension Wording {
    /// The popover block's title for a report of another schema.
    public static func otherVersionTitle(_ v: OtherVersion) -> String {
        if v.maker == .olderThanApp { return "Scan again after the update" }
        return v.updateTheApp ? "Update the app, or scan again" : "A new scan is needed"
    }

    /// Why a new scan is needed, naming who made the report. `subject`
    /// starts the sentence ("Your last scan was made by"), `object` is what
    /// this version reads differently ("reports", "it").
    public static func otherVersionText(_ v: OtherVersion, subject: String = "Your last scan was made by", object: String = "reports") -> String {
        let update = "Update DeepDriveCleaner to read it, or scan again."
        switch (v.maker, v.ddcVersion) {
        case let (.olderThanApp, version?):
            return "\(subject) ddc \(version). After the update, this version reads \(object) differently, so it needs a new scan."
        case let (.newerThanApp, version?):
            return "\(subject) ddc \(version), a newer version than this app. \(update)"
        case let (_, version?):
            return v.reportIsNewer
                ? "\(subject) ddc \(version) in a newer report format. \(update)"
                : "\(subject) ddc \(version) in an older report format, so this version needs a new scan."
        case (_, nil):
            return v.reportIsNewer
                ? "\(subject) a newer version of ddc. \(update)"
                : "\(subject) an earlier version of ddc, which this version reads differently, so it needs a new scan."
        }
    }

    /// The popover header's subtitle in place of "No scan yet".
    public static func otherVersionSubtitle(_ v: OtherVersion) -> String {
        let maker = v.ddcVersion.map { "ddc \($0)" } ?? (v.reportIsNewer ? "a newer ddc" : "an earlier ddc")
        return "Last scan by \(maker) · \(v.updateTheApp ? "update the app" : "scan again")"
    }

    /// The Whole Disk view's title for a `last-disk.json` of another schema.
    public static func otherVersionDiskTitle(_ v: OtherVersion) -> String {
        v.updateTheApp ? "Update DeepDriveCleaner, or Scan Again" : "Scan Whole Disk Again"
    }

    /// Below the block when Full Disk Access is missing, as on a first scan.
    public static let otherVersionWithoutAccess =
        "Without Full Disk Access, a scan asks for Desktop, Documents, Downloads and other apps' data, one dialog per folder."

    /// The Whole Disk view's text for a `last-disk.json` of another schema.
    public static func otherVersionDiskText(_ v: OtherVersion) -> String {
        let text = otherVersionText(v, subject: "The last whole-disk report was made by", object: "it")
        return v.updateTheApp ? text : text + " A new scan takes a few minutes."
    }
}
