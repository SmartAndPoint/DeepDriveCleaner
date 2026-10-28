import Foundation

/// The CLI's own LaunchAgent, from 0.1.0 or a 0.2.x `ddc schedule install`
/// (`crates/ddc-cli/src/launchd.rs`): a plist in `~/Library/LaunchAgents`
/// that runs `deepdrivecleaner scan --background` at 11:30, daily or on
/// Mondays. While it exists the app schedules nothing (B1) and offers to
/// replace it (B2).
public struct LegacyAgent: Sendable, Equatable {
    /// `LABEL` in `launchd.rs`.
    public static let label = "com.smartandpoint.deepdrivecleaner"
    /// The app schedules whole hours; the agent runs at 11:30.
    public static let hour = 11

    public static func plist(home: URL) -> URL {
        home.appending(path: "Library/LaunchAgents/\(label).plist")
    }

    /// `StartCalendarInterval` holds a `Weekday`.
    public let weekly: Bool
    /// `--threshold`, when the agent was installed with one.
    public let threshold: UInt64?

    /// The app's schedule after Replace It: 11:00, daily or on Mondays
    /// (launchd's Weekday 1 is `Calendar`'s 2, B5).
    public var schedule: Schedule {
        Schedule(enabled: true, repeats: weekly ? .weekly : .daily, weekday: 2, hour: Self.hour)
    }

    /// Read into memory before `schedule uninstall` deletes the file. Nil
    /// when there is no file; an unreadable one reads as daily, no threshold.
    public static func read(_ url: URL) -> LegacyAgent? {
        guard let data = try? Data(contentsOf: url) else { return nil }
        let plist = (try? PropertyListSerialization.propertyList(from: data, format: nil)) as? [String: Any] ?? [:]
        let interval = plist["StartCalendarInterval"]
        let weekly = (interval as? [String: Any])?["Weekday"] != nil
            || ((interval as? [[String: Any]])?.contains { $0["Weekday"] != nil } ?? false)
        let arguments = plist["ProgramArguments"] as? [String] ?? []
        let threshold = arguments.firstIndex(of: "--threshold").flatMap { index in
            arguments.indices.contains(index + 1) ? size(arguments[index + 1]) : nil
        }
        return LegacyAgent(weekly: weekly, threshold: threshold)
    }

    /// `parse_size` of `crates/ddc-cli/src/cli.rs`: bytes, or K, M, G, T in powers of 1024.
    static func size(_ text: String) -> UInt64? {
        let text = text.trimmingCharacters(in: .whitespaces)
        guard let last = text.last else { return nil }
        let shifts: [Character: UInt64] = ["K": 10, "M": 20, "G": 30, "T": 40]
        if let unit = String(last).uppercased().first, let shift = shifts[unit] {
            guard let value = Double(text.dropLast().trimmingCharacters(in: .whitespaces)), value >= 0 else { return nil }
            return UInt64(value * Double(UInt64(1) << shift))
        }
        return UInt64(text)
    }
}
