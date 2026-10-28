import Foundation

/// The Folders pane's "Not readable" list (artboard 10): the folders the
/// app's last Scan now could not read because the user said no when macOS
/// asked. Only System Settings can change that.
///
/// A report's `inaccessible` is collapsed to its prompting area, one row per
/// area; what lies outside every area (parts of `~/Library` without Full
/// Disk Access) is not a "no" the user gave, and is left out.
public enum NotReadable {
    /// `PROMPTING_AREAS` in `crates/ddc-cli/src/cmd/background.rs`, relative
    /// to the home folder: what a scheduled scan without Full Disk Access
    /// leaves out. A contract test reads the Rust list.
    public static let promptingAreas = [
        "Desktop",
        "Documents",
        "Downloads",
        "Pictures",
        "Movies",
        "Music",
        "Library/Containers",
        "Library/Group Containers",
        "Library/Mail",
        "Library/Messages",
        "Library/Calendars",
        "Library/Reminders",
        "Library/Application Support/AddressBook",
        "Library/Application Support/CallHistoryDB",
        "Library/Application Support/MobileSync",
    ]

    /// The areas with an unreadable folder at or under them, as absolute
    /// paths, in the order of `promptingAreas`.
    public static func areas(of report: ScanReport) -> [String] {
        let home = report.home.hasSuffix("/") ? String(report.home.dropLast()) : report.home
        return promptingAreas.map { "\(home)/\($0)" }.filter { area in
            report.inaccessible.contains { $0.path == area || $0.path.hasPrefix(area + "/") }
        }
    }
}
