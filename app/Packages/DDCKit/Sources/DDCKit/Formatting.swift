import Foundation

/// Sizes as the CLI writes them (`crates/ddc-cli/src/size.rs::human`): binary
/// units, one decimal from KiB up, so the popover, the terminal and the
/// notifications name the same number.
public enum ByteSize {
    public static func human(_ bytes: UInt64) -> String {
        let units = ["KiB", "MiB", "GiB", "TiB", "PiB"]
        if bytes < 1024 { return "\(bytes) B" }
        var value = Double(bytes) / 1024
        var unit = 0
        while value >= 1024, unit < units.count - 1 {
            value /= 1024
            unit += 1
        }
        return String(format: "%.1f %@", value, units[unit])
    }
}

/// The popover's words for counts, times and places (artboard 1 of the canvas).
public enum Wording {
    /// "5.5 M", "12.3 K", "980": the canvas's form, not `size.rs::count`.
    public static func count(_ n: UInt64) -> String {
        switch n {
        case 0..<1_000: "\(n)"
        case 1_000..<1_000_000: String(format: "%.1f K", Double(n) / 1e3)
        default: String(format: "%.1f M", Double(n) / 1e6)
        }
    }

    /// "39.6 s", "2 min 5 s".
    public static func duration(milliseconds: UInt64) -> String {
        if milliseconds < 60_000 { return String(format: "%.1f s", Double(milliseconds) / 1000) }
        let seconds = milliseconds / 1000
        return "\(seconds / 60) min \(seconds % 60) s"
    }

    /// "2,007": a count of items, grouped as the canvas writes it.
    public static func number(_ n: Int) -> String {
        n.formatted(.number.grouping(.automatic).locale(Locale(identifier: "en_US")))
    }

    /// "1 item", "2,007 items".
    public static func items(_ n: Int, _ noun: String = "item") -> String {
        "\(number(n)) \(n == 1 ? noun : noun + "s")"
    }

    /// A group's own clean (story "Review window: clean only the selection
    /// in this group"): "Clean 2 from Browser and app caches…".
    public static func cleanFrom(_ count: Int, group: String) -> String {
        "Clean \(number(count)) from \(group)…"
    }

    /// A User data folder's own button (story "User data: move a folder's
    /// picked files to the Trash from that folder"): "Move 2 from Movies to
    /// the Trash…"; with nothing picked, "Move to the Trash…".
    public static func moveFrom(_ count: Int, group: String) -> String {
        count == 0 ? "Move to the Trash…" : "Move \(number(count)) from \(group) to the Trash…"
    }

    /// That folder's row: "2 picked · 3.1 GiB", or what to do with nothing
    /// picked: "files", or a coding agent's "sessions".
    public static func picked(count: Int, bytes: UInt64, what: String = "files") -> String {
        count == 0 ? "Pick \(what) below to move them to the Trash" : "\(number(count)) picked · \(ByteSize.human(bytes))"
    }

    /// What a group's Select all row acts on: "2 of 3 · 7.2 GiB"; with
    /// nothing selected, "0 of 3".
    public static func scopeSummary(selected: Int, of total: Int, bytes: UInt64) -> String {
        let counts = "\(number(selected)) of \(number(total))"
        return selected == 0 ? counts : "\(counts) · \(ByteSize.human(bytes))"
    }

    /// "Spotify", "Spotify and Chrome", "Spotify, Chrome and Brave".
    public static func list(_ words: [String]) -> String {
        guard let last = words.last else { return "" }
        if words.count == 1 { return last }
        return words.dropLast().joined(separator: ", ") + " and " + last
    }

    /// `render::tilde` in `crates/ddc-cli/src/render.rs`: `/Users/me/x` → `~/x`,
    /// control characters escaped so a crafted name cannot disguise itself.
    public static func tilde(_ path: String, home: String) -> String {
        let shown: String
        if path == home {
            shown = "~"
        } else if !home.isEmpty, path.hasPrefix(home.hasSuffix("/") ? home : home + "/") {
            shown = "~/" + path.dropFirst(home.hasSuffix("/") ? home.count : home.count + 1)
        } else {
            shown = path
        }
        return escaped(shown)
    }

    /// Control characters and bidirectional format characters escaped, so a
    /// crafted name cannot disguise itself: `cod\u{202E}fdp.exe` would read as a PDF.
    public static func escaped(_ text: String) -> String {
        text.unicodeScalars.map { scalar in
            scalar.properties.generalCategory == .control || bidi(scalar) ? scalar.escaped(asASCII: true) : String(scalar)
        }.joined()
    }

    /// Embeddings, overrides and isolates (U+202A–U+202E, U+2066–U+2069) and
    /// the direction marks (U+200E, U+200F, U+061C).
    static func bidi(_ scalar: Unicode.Scalar) -> Bool {
        switch scalar.value {
        case 0x202A...0x202E, 0x2066...0x2069, 0x200E, 0x200F, 0x061C: true
        default: false
        }
    }

    /// "9.2 of 13.1 GiB", the unit said once when both share it, when part is
    /// selected; else the total, also when both round to the same text.
    public static func amount(selected: UInt64, total: UInt64) -> String {
        let (part, whole) = (ByteSize.human(selected), ByteSize.human(total))
        if selected == 0 || part == whole { return whole }
        let unit = { (text: String) in text.split(separator: " ").last.map(String.init) ?? "" }
        let shown = unit(part) == unit(whole) ? String(part.dropLast(unit(part).count + 1)) : part
        return "\(shown) of \(whole)"
    }

    /// "6.2 of 13.9 GiB", "0 B of 13.9 GiB": a clean's progress, the unit
    /// said once when both share it; the part is always named.
    public static func progress(_ done: UInt64, of total: UInt64) -> String {
        let (part, whole) = (ByteSize.human(done), ByteSize.human(total))
        let unit = { (text: String) in text.split(separator: " ").last.map(String.init) ?? "" }
        let shown = unit(part) == unit(whole) ? String(part.dropLast(unit(part).count + 1)) : part
        return "\(shown) of \(whole)"
    }

    /// "17 s", "2 min 5 s": the running scan's elapsed time.
    public static func elapsed(_ interval: TimeInterval) -> String {
        let seconds = max(0, Int(interval))
        return seconds < 60 ? "\(seconds) s" : "\(seconds / 60) min \(seconds % 60) s"
    }

    /// "just now", "5 min ago", "2 h ago", "3 d ago".
    public static func age(of date: Date, now: Date) -> String {
        let minutes = Int(now.timeIntervalSince(date) / 60)
        switch minutes {
        case ..<1: return "just now"
        case ..<60: return "\(minutes) min ago"
        case ..<(48 * 60): return "\(minutes / 60) h ago"
        default: return "\(minutes / (24 * 60)) d ago"
        }
    }

    /// `generated_at` of a report (RFC 3339, with or without fractions).
    public static func date(rfc3339 text: String) -> Date? {
        if let date = try? Date(text, strategy: .iso8601) { return date }
        return try? Date(text, strategy: Date.ISO8601FormatStyle(includingFractionalSeconds: true))
    }

    /// `volume_name` in `crates/ddc-cli/src/notify.rs` for the startup disk;
    /// another volume by its name.
    public static func volume(_ mountPoint: String) -> String {
        switch mountPoint {
        case "/", "/System/Volumes/Data": "The startup disk"
        default: URL(filePath: mountPoint).lastPathComponent
        }
    }

    /// While the 0.1.0 agent runs, the app schedules nothing (B1).
    public static let replaceOldScanFirst = "Replace the old background scan first"

    /// Where the report looked: "in your home folder", "in Work", "in 3 folders".
    public static func scope(roots: [String], home: String) -> String {
        switch roots.count {
        case 0: return "in your home folder"
        case 1 where roots[0] == home: return "in your home folder"
        case 1:
            let root = roots[0]
            let parts = root.split(separator: "/")
            let name = parts.count == 2 && parts[0] == "Volumes" ? String(parts[1]) : URL(filePath: root).lastPathComponent
            return "in \(name)"
        default: return "in \(roots.count) folders"
        }
    }

    /// The notice for a report that is not of the app's roots
    /// (`ReportScope`); nil for one that is. One root by its path, several
    /// counted as `scope` counts them. A report made under another home
    /// (`reportHome`) says so: its `~` is not the user's.
    public static func reportScope(_ scope: ReportScope, roots: [String], home: String, reportHome: String? = nil) -> String? {
        if scope == .outside, let reportHome, ReportScope.canonical(reportHome) != ReportScope.canonical(home) {
            return "This report is of another home folder, \(escaped(reportHome)), not of yours. Scan Now replaces it."
        }
        let what = roots.count == 1 ? tilde(roots[0], home: home) : "\(roots.count) folders"
        switch scope {
        case .same: return nil
        case .inside: return "This report is of \(what) only. Scan Now covers your home folder and the folders in Settings."
        case .outside: return "This report is of \(what), not of your home folder and the folders in Settings. Scan Now replaces it."
        }
    }

    /// The footer's next scheduled scan: "Next scan today, 13:00",
    /// "Next scan tomorrow, 10:00", "Next scan Monday, 10:00", "Next scan
    /// within the hour" when one is due (a missed slot, a Resume), "Scheduled
    /// scans off", or "Replace the old background scan first" while the 0.1.0
    /// agent runs. The time follows the user's 12- or 24-hour clock.
    public static func nextScan(_ scan: NextScan, now: Date, calendar: Calendar = .current, locale: Locale = .autoupdatingCurrent) -> String {
        let next: Date
        switch scan {
        case .off: return "Scheduled scans off"
        case .replaceOldScanFirst: return replaceOldScanFirst
        case .at(let date): next = date
        }
        if next <= now { return "Next scan within the hour" }
        let time = next.formatted(Date.FormatStyle(date: .omitted, time: .shortened, locale: locale, calendar: calendar, timeZone: calendar.timeZone))
        let day: String
        if calendar.isDate(next, inSameDayAs: now) {
            day = "today"
        } else if let tomorrow = calendar.date(byAdding: .day, value: 1, to: now), calendar.isDate(next, inSameDayAs: tomorrow) {
            day = "tomorrow"
        } else {
            let weekday = calendar.component(.weekday, from: next)
            day = calendar.weekdaySymbols[weekday - 1]
        }
        return "Next scan \(day), \(time)"
    }
}
