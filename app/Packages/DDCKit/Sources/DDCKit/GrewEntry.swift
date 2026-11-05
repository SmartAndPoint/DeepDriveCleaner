import Foundation

/// The Review window's "Grew since …" entry (story "Popover and Review: what
/// grew since the earlier scan", DDC-002): a view over the window's own rows,
/// the ones a `changes` result says are new, grew or moved, with their
/// change, and what no row explains. A pure value, so every part is a unit
/// test without SwiftUI. Its rows are `ReviewItems` rows, so a tick here is
/// the same tick in their group; nothing here starts a clean.
public struct GrewEntry: Sendable, Equatable {
    /// The entry's order: Change (the default, biggest first), Size or Name.
    public struct Sort: Sendable, Equatable {
        public enum Column: Sendable, Equatable, CaseIterable {
            case name, size, change
        }

        public let column: Column
        public let ascending: Bool

        public init(column: Column, ascending: Bool) {
            self.column = column
            self.ascending = ascending
        }

        public static let `default` = Sort(column: .change, ascending: false)

        /// The sorted column reverses; another starts biggest first, Name A to Z.
        public func clicking(_ column: Column) -> Sort {
            column == self.column ? Sort(column: column, ascending: !ascending) : Sort(column: column, ascending: column == .name)
        }
    }

    public struct Row: Sendable, Equatable, Identifiable {
        /// The `ReviewItems` row's id.
        public let id: Int
        /// "new", "+3.1 GiB", "moved", with "≈ " before an approximate one.
        public let change: String
        public let delta: Int64
    }

    public struct Group: Sendable, Equatable, Identifiable {
        public let entry: ReviewItems.Entry
        public let rows: [Row]
        public let delta: Int64
        /// The rows its header checkbox ticks: only where the group's own entry
        /// has Select all, and only the rows shown here.
        public let selectAll: [Int]?

        public var id: ReviewItems.Entry { entry }
    }

    /// A kind of what the current scan did not read, with its count.
    public struct NotRead: Sendable, Equatable {
        public let kind: ChangesReport.NotReadKind
        public let count: Int
        /// "2 cloud folders: not read by any scan."
        public let text: String
    }

    public struct Gone: Sendable, Equatable, Identifiable {
        public let path: String
        public let kind: ChangesReport.ItemKind
        /// "cleaned 3.0 GiB", "gone", "−1.2 GiB".
        public let text: String

        public let rule: String

        public var id: String { "\(path)\u{0}\(rule)" }
    }

    /// "Grew since yesterday 10:04".
    public let title: String
    /// The sidebar's number: "+191.0 GiB", "+188.0 GiB in findings", or nil.
    public let amount: String?
    /// The levels in one sentence, or why the home total did not compare.
    public let summary: String
    /// "Compared in ~/Projects only: the earlier scan covered other folders."
    public let rootsNote: String?
    public let groups: [Group]
    /// Growth in folders that are no finding, when the home compared and it grew.
    public let elsewhere: String?
    /// What the disk's growth leaves after the home, when positive.
    public let rest: String?
    public let notRead: [NotRead]
    /// The scan ran without Full Disk Access: Open Privacy Settings helps.
    public let offersPrivacy: Bool
    /// The person cleaned since: their bytes can read as `rest` for a while.
    public let cleanedNote: String?
    public let gone: [Gone]
    /// "You cleaned 40.0 GiB since then; 12.0 GiB of it is in the Trash."
    public let cleanedSince: String?

    /// Nil when there is nothing to show: not compared, or no matched row
    /// grew and the CLI does not say `grew`.
    public static func make(
        items: ReviewItems,
        changes: ChangesReport,
        sort: Sort = .default,
        home: String,
        now: Date,
        calendar: Calendar = .current,
        locale: Locale = .autoupdatingCurrent
    ) -> GrewEntry? {
        guard changes.status == .compared else { return nil }
        // By path bytes and rule: two spellings of one name stay two.
        struct Key: Hashable {
            let path: [UInt8]
            let rule: String
        }
        let rowsByKey = Dictionary(
            items.rows.filter { $0.userEntry == nil && $0.lock != .typeOff && $0.lock != .underMinimumSize }
                .map { (Key(path: Array($0.finding.path.utf8), rule: $0.finding.rule), $0) },
            uniquingKeysWith: { first, _ in first }
        )
        var byEntry: [ReviewItems.Entry: [(ReviewItems.Row, ChangesReport.Item)]] = [:]
        for item in changes.items where [.new, .grew, .moved].contains(item.kind) && !item.ignored {
            guard let row = rowsByKey[Key(path: Array(item.path.utf8), rule: item.rule)] else { continue }
            byEntry[ReviewItems.Entry(riskClass: row.riskClass, group: row.group), default: []].append((row, item))
        }
        guard !byEntry.isEmpty || changes.grew else { return nil }

        let groups = byEntry.map { entry, pairs -> Group in
            let rows = order(pairs, sort: sort, home: home).map { row, item in
                Row(id: row.id, change: change(item), delta: item.delta)
            }
            let allowed = items.layout(entry, matching: "").selectAll.map(Set.init)
            let selectAll = allowed.map { allowed in rows.map(\.id).filter(allowed.contains) }.flatMap { $0.isEmpty ? nil : $0 }
            return Group(entry: entry, rows: rows, delta: pairs.reduce(0) { $0 + $1.1.delta }, selectAll: selectAll)
        }
        .sorted {
            if $0.delta != $1.delta { return $0.delta > $1.delta }
            if $0.entry.group != $1.entry.group { return $0.entry.group < $1.entry.group }
            return $0.entry.riskClass.rawValue < $1.entry.riskClass.rawValue
        }

        let when = changes.baseline.flatMap { $0.writtenAt ?? $0.generatedAt }.flatMap(Wording.date(rfc3339:))
            .map { Wording.since($0, now: now, calendar: calendar, locale: locale) }
        let title = when.map { "Grew since \($0)" } ?? "Grew since the earlier scan"
        let positive = changes.groups.reduce(Int64(0)) { $0 + max($1.delta, 0) }
        let compared = changes.home?.compared == true
        let amount: String?
        if compared, let delta = changes.home?.delta, delta > 0 {
            amount = "+\(ByteSize.human(UInt64(delta)))"
        } else if positive > 0 {
            amount = "+\(ByteSize.human(UInt64(positive))) in findings"
        } else {
            amount = nil
        }

        var notRead: [ChangesReport.NotReadKind: Int] = [:]
        for path in changes.rest?.notRead ?? [] { notRead[path.kind, default: 0] += 1 }
        let cleaned = changes.cleanedSince?.total ?? 0
        let restDelta = changes.rest?.delta ?? 0
        let gone = changes.items.filter { [.cleaned, .gone, .shrank].contains($0.kind) && !$0.ignored }.map {
            Gone(path: Wording.tilde($0.path, home: home), kind: $0.kind, text: goneText($0), rule: $0.rule)
        }
        return GrewEntry(
            title: title,
            amount: amount,
            summary: summary(changes),
            rootsNote: rootsNote(changes, roots: items.report.scanRoots, home: home),
            groups: groups,
            elsewhere: compared ? changes.home?.elsewhere.flatMap { $0 > 0 ? "+\(ByteSize.human(UInt64($0)))" : nil } : nil,
            rest: restDelta > 0 ? "+\(ByteSize.human(UInt64(restDelta)))" : nil,
            notRead: ChangesReport.NotReadKind.allCases.compactMap { kind in
                notRead[kind].map { NotRead(kind: kind, count: $0, text: notReadText(kind, count: $0, withoutAccess: !items.report.fullDiskAccess)) }
            },
            offersPrivacy: !items.report.fullDiskAccess && notRead[.prompting, default: 0] > 0,
            cleanedNote: cleaned > 0 ? "Space you cleaned can read here until macOS frees it (local snapshots hold it)." : nil,
            gone: gone,
            cleanedSince: changes.cleanedSince.flatMap { since in
                guard since.total > 0 else { return nil }
                let trash = since.trashed > 0 ? "; \(ByteSize.human(since.trashed)) of it is in the Trash" : ""
                return "You cleaned \(ByteSize.human(since.total)) since then\(trash)."
            }
        )
    }

    /// The entry as the window's filter shows it: the rows that match
    /// `query` (`ReviewItems.matches`), Select all over those only, groups
    /// left with none dropped. The sections stay.
    public func filtered(items: ReviewItems, query: String) -> GrewEntry {
        guard !query.trimmingCharacters(in: .whitespaces).isEmpty else { return self }
        let groups = groups.compactMap { group -> Group? in
            let rows = group.rows.filter { items.row($0.id).map { items.matches($0, query) } ?? false }
            guard !rows.isEmpty else { return nil }
            let shown = Set(rows.map(\.id))
            let selectAll = group.selectAll.map { $0.filter(shown.contains) }.flatMap { $0.isEmpty ? nil : $0 }
            return Group(entry: group.entry, rows: rows, delta: group.delta, selectAll: selectAll)
        }
        return GrewEntry(
            title: title, amount: amount, summary: summary, rootsNote: rootsNote, groups: groups, elsewhere: elsewhere, rest: rest,
            notRead: notRead, offersPrivacy: offersPrivacy, cleanedNote: cleanedNote, gone: gone, cleanedSince: cleanedSince
        )
    }

    /// The rows in `sort`'s order; ties by path as shown.
    private static func order(_ pairs: [(ReviewItems.Row, ChangesReport.Item)], sort: Sort, home: String) -> [(ReviewItems.Row, ChangesReport.Item)] {
        pairs.sorted { a, b in
            let nameA = Wording.tilde(a.0.finding.path, home: home)
            let nameB = Wording.tilde(b.0.finding.path, home: home)
            switch sort.column {
            case .change where a.1.delta != b.1.delta:
                return (a.1.delta < b.1.delta) == sort.ascending
            case .size where a.0.finding.bytes != b.0.finding.bytes:
                return (a.0.finding.bytes < b.0.finding.bytes) == sort.ascending
            case .name:
                let order = nameA.localizedStandardCompare(nameB)
                if order != .orderedSame { return (order == .orderedAscending) == sort.ascending }
            default:
                break
            }
            return nameA.localizedStandardCompare(nameB) == .orderedAscending
        }
    }

    /// "new", "+3.1 GiB", "moved"; "≈ " before one that shares bytes.
    static func change(_ item: ChangesReport.Item) -> String {
        let text: String
        switch item.kind {
        case .new: text = "new"
        case .moved: text = "moved"
        default: text = item.delta >= 0 ? "+\(ByteSize.human(UInt64(item.delta)))" : "−\(ByteSize.human(UInt64(-item.delta)))"
        }
        return item.approximate ? "≈ \(text)" : text
    }

    private static func goneText(_ item: ChangesReport.Item) -> String {
        switch item.kind {
        case .cleaned: "cleaned \(ByteSize.human(item.cleaned))"
        case .gone: "gone"
        default: "−\(ByteSize.human(UInt64(max(-item.delta, 0))))"
        }
    }

    /// One sentence of the levels, or why the home total did not compare.
    private static func summary(_ changes: ChangesReport) -> String {
        guard let home = changes.home, home.compared, let homeDelta = home.delta else {
            if changes.home?.reasons.contains(.differentAccess) == true {
                return "Home total not compared: one scan had Full Disk Access and the other not."
            }
            return "Home total not compared: the two scans read different folders."
        }
        guard let disk = changes.disk else {
            return "\(signed(homeDelta)) in your home folder."
        }
        let volume = Wording.volumeMidSentence(disk.mountPoint)
        let rest = changes.rest?.delta ?? 0
        let head = disk.delta >= 0
            ? "\(ByteSize.human(UInt64(disk.delta))) more in use on \(volume)"
            : "\(ByteSize.human(UInt64(-disk.delta))) less in use on \(volume)"
        return "\(head): \(signed(homeDelta)) in your home folder, \(signed(rest)) in folders ddc did not read or outside your home folder."
    }

    private static func signed(_ delta: Int64) -> String {
        delta >= 0 ? ByteSize.human(UInt64(delta)) : "−\(ByteSize.human(UInt64(-delta)))"
    }

    /// A group's change: "+186.0 GiB", "−2.0 GiB".
    public static func amount(_ delta: Int64) -> String {
        delta >= 0 ? "+\(ByteSize.human(UInt64(delta)))" : "−\(ByteSize.human(UInt64(-delta)))"
    }

    private static func rootsNote(_ changes: ChangesReport, roots: [String], home: String) -> String? {
        guard !changes.comparedRoots.isEmpty, Set(changes.comparedRoots) != Set(roots) else { return nil }
        let shown = changes.comparedRoots.map { Wording.tilde($0, home: home) }.joined(separator: ", ")
        return "Compared in \(shown) only: the earlier scan covered other folders."
    }

    private static func notReadText(_ kind: ChangesReport.NotReadKind, count: Int, withoutAccess: Bool) -> String {
        let folders = Wording.items(count, "folder")
        switch kind {
        case .cloud: return "\(folders) in the cloud: not read by any scan."
        case .skip: return "\(folders) you skip in Settings."
        case .prompting: return "\(folders) macOS asks about before reading" + (withoutAccess ? ": the scan had no Full Disk Access." : ".")
        case .refused: return "\(folders) the system refused to list."
        case .evicted: return "\(folders) whose files are in the cloud only."
        case .otherDisk: return "\(folders) on another disk."
        case .snapshot: return "\(Wording.items(count, "snapshot")) of the file system."
        case .changed: return "\(folders) that changed while the scan read them."
        }
    }
}
