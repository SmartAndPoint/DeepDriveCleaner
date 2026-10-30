import Foundation

/// What the Whole disk view's Coding agents section draws from an
/// `AgentsReport` (story "Coding agents in the Whole disk view"): a row per
/// harness, then its folders, its live sessions, its remaining transcripts.
/// It names and sizes; nothing here removes anything.
public struct AgentsSummary: Sendable, Equatable {
    public enum Kind: Sendable, Equatable {
        case harness, folder, session, transcripts
        /// "ddc cannot read Claude Code's session records".
        case records
    }

    public struct Row: Sendable, Equatable, Identifiable {
        public let id: String
        public let kind: Kind
        public let title: String
        /// "12.0 GiB", "at least 2.0 GiB", "skipped", or empty before the sizes.
        public let size: String
        public let note: String
        /// What Show in Finder selects: a folder its own path, a harness its
        /// top-level folders, anything else nothing (story "Whole disk: Show
        /// in Finder and the honest action for each row").
        public let paths: [String]
    }

    /// False while only the `--no-sizes` report is in: sizes read empty.
    public let sizes: Bool
    public let rows: [Row]

    /// Paths under `home` read `~/…`. Every text a harness wrote is escaped.
    public static func make(
        _ report: AgentsReport,
        home: String,
        now: Date,
        calendar: Calendar = .current,
        locale: Locale = .autoupdatingCurrent
    ) -> AgentsSummary {
        var rows: [Row] = []
        for harness in report.harnesses {
            let id = harness.id.rawValue
            let tops = harness.folders.filter { !$0.nested }
            let skipped = !tops.isEmpty && tops.allSatisfy(\.skipped)
            rows.append(Row(
                id: id,
                kind: .harness,
                title: harness.name,
                size: skipped ? "skipped" : size(harness.bytes, sizes: report.sizes, unreadable: harness.folders.contains { $0.unreadable > 0 }),
                note: skipped ? "Skipped in Settings" : running(harness.running),
                paths: tops.map(\.path)
            ))
            for folder in harness.folders {
                rows.append(Row(
                    id: "\(id)/folder/\(folder.path)",
                    kind: .folder,
                    title: Wording.tilde(folder.path, home: home),
                    size: folder.skipped ? "skipped" : size(folder.bytes, sizes: report.sizes, unreadable: folder.unreadable > 0),
                    note: folder.skipped
                        ? "Skipped in Settings"
                        : folder.unreadable > 0 ? "\(Wording.count(folder.unreadable)) unreadable" : "",
                    paths: [folder.path]
                ))
            }
            guard let sessions = harness.sessions else { continue }
            if sessions.records == .unknown {
                rows.append(Row(
                    id: "\(id)/records",
                    kind: .records,
                    title: "ddc cannot read \(harness.name)'s session records",
                    size: "",
                    note: reason(sessions.reason),
                    paths: []
                ))
            }
            for (index, session) in sessions.list.enumerated() where session.state == .live {
                rows.append(Row(
                    // One session can be open twice: the index keeps the ids apart.
                    id: "\(id)/session/\(index)/\(session.id ?? "")",
                    kind: .session,
                    title: project(session),
                    size: "",
                    note: sessionNote(session, harness: harness.id, now: now, calendar: calendar, locale: locale),
                    paths: []
                ))
            }
            if let line = transcripts(sessions.transcripts, sizes: report.sizes) {
                rows.append(Row(id: "\(id)/transcripts", kind: .transcripts, title: line.title, size: line.size, note: line.note, paths: []))
            }
        }
        return AgentsSummary(sizes: report.sizes, rows: rows)
    }

    private static func size(_ bytes: UInt64?, sizes: Bool, unreadable: Bool) -> String {
        guard sizes, let bytes else { return "" }
        return unreadable ? "at least \(ByteSize.human(bytes))" : ByteSize.human(bytes)
    }

    /// "Runs: 2 CLI sessions and the app server", "Not running".
    static func running(_ running: [AgentsReport.Running]) -> String {
        guard !running.isEmpty else { return "Not running" }
        let words = AgentsReport.RunningKind.allCases.compactMap { kind -> String? in
            let n = running.filter { $0.kind == kind }.count
            guard n > 0 else { return nil }
            switch kind {
            case .cli: return n == 1 ? "the CLI" : "\(n) CLI processes"
            case .appServer: return n == 1 ? "the app server" : "\(n) app servers"
            case .app: return "the app"
            case .other: return n == 1 ? "1 other process" : "\(n) other processes"
            }
        }
        return "Runs: " + Wording.list(words)
    }

    /// The project folder's name, else the session's name.
    private static func project(_ session: AgentsReport.Session) -> String {
        if let cwd = session.cwd, !cwd.isEmpty {
            let name = URL(filePath: cwd).lastPathComponent
            return Wording.escaped(name.isEmpty ? cwd : name)
        }
        if let name = session.name, !name.isEmpty { return Wording.escaped(name) }
        return "Session"
    }

    private static func sessionNote(
        _ session: AgentsReport.Session,
        harness: AgentsReport.HarnessID,
        now: Date,
        calendar: Calendar,
        locale: Locale
    ) -> String {
        var parts: [String] = []
        if harness == .codex { parts.append("open in Codex") }
        if session.cwd != nil, let name = session.name, !name.isEmpty { parts.append(Wording.escaped(name)) }
        if let status = session.status, !status.isEmpty { parts.append(Wording.escaped(status)) }
        if let started = session.startedAt.flatMap(Wording.date(rfc3339:)) {
            parts.append("started \(Wording.moment(started, now: now, calendar: calendar, locale: locale))")
        } else if let time = session.rolloutTime {
            // As written in the rollout's name; its time zone is not settled.
            parts.append("started \(Wording.escaped(rollout(time)))")
        }
        return parts.joined(separator: " · ")
    }

    /// `2026-10-28T09-41-07` → `2026-10-28 09:41:07`; anything else as is.
    static func rollout(_ time: String) -> String {
        let halves = time.split(separator: "T", maxSplits: 1)
        guard halves.count == 2 else { return time }
        return "\(halves[0]) \(halves[1].replacingOccurrences(of: "-", with: ":"))"
    }

    private static func transcripts(_ t: AgentsReport.Transcripts, sizes: Bool) -> (title: String, size: String, note: String)? {
        if t.skipped {
            return ("Transcripts not read", "", "A folder that holds them is skipped in Settings")
        }
        guard let count = t.count, count > 0 else { return nil }
        let title = "\(Wording.number(Int(clamping: count))) \(count == 1 ? "transcript" : "transcripts") of ended sessions"
        return (title, size(t.bytes, sizes: sizes, unreadable: false), "")
    }

    private static func reason(_ reason: AgentsReport.UnknownReason?) -> String {
        switch reason {
        case .missing: "Its records folder is missing."
        case .unreadable: "Its records folder could not be read."
        case .unparsed: "No record has the shape ddc knows; the format may have changed."
        case .skipped: "Its folder is skipped in Settings."
        case nil: ""
        }
    }
}
