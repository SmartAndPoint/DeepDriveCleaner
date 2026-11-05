import Foundation

/// The scan after an update (story "Scan after an update when the saved
/// report cannot be read", DDC-002; menu bar ADR §5, amended 2026-11-05):
/// after almost every update the saved report is of an older schema, which
/// the app reads as no report. At the first launch of a new build, with
/// scheduled scans on and not paused, the scheduled scan is made due at once.
public enum UpdateScan {
    /// What `last-scan.json` is to this build, by its `schema_version` alone.
    public enum SavedReport: Sendable, Equatable {
        case none
        case readable
        case older(Int)
        case newer(Int)
        /// Not JSON, or no version: nothing to decide from.
        case unreadable
    }

    /// Reads only the header of the saved report.
    public static func savedReport(at url: URL) async -> SavedReport {
        guard FileManager.default.fileExists(atPath: url.path) else { return .none }
        // Several MB on a full home: decoded off the main actor.
        return await Task.detached {
            guard let data = try? Data(contentsOf: url),
                  let version = try? JSONDecoder.snakeCase.decode(VersionHeader.self, from: data).schemaVersion
            else { return SavedReport.unreadable }
            if version == ScanReport.schemaVersion { return .readable }
            return version < ScanReport.schemaVersion ? .older(version) : .newer(version)
        }.value
    }

    /// Whether the scan after the update is due: a new build (or one with no
    /// record of the last, the first build that writes it), a report of an
    /// older schema, scheduled scans on and not paused, and no 0.1.0 agent.
    /// Not for no report (a first launch stays the person's Scan Now), a
    /// report that reads, or one of a newer `ddc`.
    public static func needed(buildChanged: Bool, report: SavedReport, scheduleEnabled: Bool, paused: Bool, legacyAgentFound: Bool) -> Bool {
        guard buildChanged, scheduleEnabled, !paused, !legacyAgentFound, case .older = report else { return false }
        return true
    }
}
