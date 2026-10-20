import Foundation

/// A checkbox over several rows: Select all, or a subgroup's heading.
public enum CheckState: Sendable, Equatable {
    case off, mixed, on
}

/// The Groups view of one sidebar entry (story "Review window: sorted
/// sidebar, Select all and folder groups"; board 9 of the canvas).
///
/// A group of one rule falls into folders, file-manager style; a group of
/// several rules has one subgroup per rule. What every row shares is said
/// once, on the heading or in the group's facts, so a row keeps only its own.
/// The layout is built from the rows alone: ticking never rebuilds it.
public struct GroupLayout: Sendable, Equatable {
    public enum Shape: Sendable, Equatable {
        /// One rule: subgroups by folder.
        case folders
        /// Several rules: a subgroup per rule.
        case rules
    }

    /// A subgroup across rebuilds, for its folded state: its folder's bytes
    /// or its full rule id, never `String` equality, so two spellings of one
    /// name, or `rust.target` and `maven.target`, stay two.
    public struct Key: Hashable, Sendable {
        public let entry: ReviewItems.Entry
        let tag: [UInt8]
    }

    public struct Item: Identifiable, Sendable, Equatable {
        public let row: ReviewItems.Row
        /// The path as the row shows it: relative to its folder, else under `~`.
        public let shown: String
        /// Only the row's own facts.
        public let detail: String
        public var id: Int { row.id }
    }

    public struct Subgroup: Sendable, Equatable {
        public enum Kind: Hashable, Sendable {
            case folder
            /// Findings the report notes as inside an app's own folder.
            case appOwned
            case rule(String)
        }

        public let key: Key
        public let kind: Kind
        /// "~/temp", "Inside apps' own folders", "npm".
        public let title: String
        /// "5 folders", "cache.npm · 2 folders · Delete".
        public let subtitle: String
        /// The rule's way back (rules shape).
        public let hint: String?
        /// Whether its heading has a checkbox: never for user data.
        public let checkable: Bool
        /// Largest first.
        public let items: [Item]

        public var bytes: UInt64 { items.reduce(0) { $0 + $1.row.finding.bytes } }
        public var ids: [Int] { items.map(\.id) }
        /// A folder subgroup, drawn with a folder icon.
        public var isFolder: Bool { if case .rule = kind { false } else { true } }
    }

    /// A rule with one item: one row, with no subgroup around it.
    public struct Single: Sendable, Equatable {
        public let title: String
        public let subtitle: String
        /// The rule's way back.
        public let hint: String?
        public let item: Item

        /// Its second line: the row's own facts, then the way back.
        public var detail: String {
            [item.detail, hint ?? ""].filter { !$0.isEmpty }.joined(separator: " · ")
        }
    }

    public enum Block: Sendable, Equatable {
        case subgroup(Subgroup)
        case single(Single)

        var bytes: UInt64 {
            switch self {
            case .subgroup(let subgroup): subgroup.bytes
            case .single(let single): single.item.row.finding.bytes
            }
        }

        var tag: [UInt8] {
            switch self {
            case .subgroup(let subgroup): subgroup.key.tag
            case .single(let single): Array(single.item.row.finding.rule.utf8)
            }
        }
    }

    /// What the view draws, in order.
    public enum Line: Identifiable, Sendable, Equatable {
        case heading(Subgroup, expanded: Bool)
        case item(Item)
        case single(Single)

        public enum ID: Hashable, Sendable {
            case heading(Key)
            case row(Int)
        }

        public var id: ID {
            switch self {
            case .heading(let subgroup, _): .heading(subgroup.key)
            case .item(let item): .row(item.id)
            case .single(let single): .row(single.item.id)
            }
        }
    }

    public let entry: ReviewItems.Entry
    public let shape: Shape
    /// What the class means: `CleanWording.classBlurb`.
    public let blurb: String
    /// What every row shares (folders shape): "python.venv · Move to the Trash · No lock file".
    public let facts: String?
    /// The rule's way back (folders shape).
    public let hint: String?
    /// The rows Select all covers; nil where the group has none (user data).
    public let selectAll: [Int]?
    /// The rows shown, and their bytes.
    public let count: Int
    public let bytes: UInt64
    /// Largest first.
    public let blocks: [Block]

    /// The lines with folded subgroups showing their heading only.
    public func lines(collapsed: Set<Key>) -> [Line] {
        blocks.flatMap { block -> [Line] in
            switch block {
            case .single(let single):
                return [.single(single)]
            case .subgroup(let subgroup):
                let expanded = !collapsed.contains(subgroup.key)
                return [.heading(subgroup, expanded: expanded)] + (expanded ? subgroup.items.map { .item($0) } : [])
            }
        }
    }
}

extension ReviewItems {
    /// The entry's sum of bytes, ignored findings left out: the sidebar's order.
    public func total(_ entry: Entry) -> UInt64 {
        rows(in: entry).reduce(0) { $0 + $1.finding.bytes }
    }

    /// The Groups view of `entry`, its rows filtered by `query`. The shape,
    /// and whether a rule is a single row, come from every row of the entry,
    /// so filtering only removes items.
    public func layout(_ entry: Entry, matching query: String) -> GroupLayout {
        let all = rows(in: entry)
        let shown = all.filter { matches($0, query) }
        let rules = Dictionary(grouping: all, by: { $0.finding.rule })
        let checkable = entry.riskClass != .userData
        let blocks: [GroupLayout.Block]
        let facts: String?
        let hint: String?
        let shape: GroupLayout.Shape
        if rules.count <= 1 {
            shape = .folders
            // What every row of the group shares: said once. App-owned rows
            // have a subgroup that says so.
            let shared = Self.sharedNotes(all).subtracting([.appOwned])
            let rule = all.first?.finding.rule
            let noted = ScanReport.Note.allCases.filter(shared.contains).map(CleanWording.note)
            facts = rule.map { ([$0, CleanWording.actionText(all)] + noted).joined(separator: " · ") }
            hint = all.first.flatMap { CleanWording.hint($0.finding.hint) }
            blocks = folderBlocks(entry, shown, omitting: shared, checkable: checkable)
        } else {
            shape = .rules
            facts = nil
            hint = nil
            blocks = ruleBlocks(entry, shown, rules: rules, checkable: checkable)
        }
        return GroupLayout(
            entry: entry,
            shape: shape,
            blurb: CleanWording.classBlurb(entry.riskClass),
            facts: facts,
            hint: hint,
            selectAll: checkable ? shown.map(\.id) : nil,
            count: shown.count,
            bytes: shown.reduce(0) { $0 + $1.finding.bytes },
            blocks: Self.ordered(blocks)
        )
    }

    private func folderBlocks(_ entry: Entry, _ rows: [Row], omitting shared: Set<ScanReport.Note>, checkable: Bool) -> [GroupLayout.Block] {
        var folders: [[UInt8]: (title: String, kind: GroupLayout.Subgroup.Kind, items: [GroupLayout.Item])] = [:]
        for row in rows {
            let appOwned = row.finding.notes.contains(.appOwned)
            let place = appOwned ? nil : Self.folder(of: row.finding.path, home: report.home)
            let tag = place.map { Array("f".utf8) + Array($0.title.utf8) } ?? Array("a".utf8)
            let item = GroupLayout.Item(
                row: row,
                shown: place?.shown ?? Wording.tilde(row.finding.path, home: report.home),
                detail: CleanWording.ownDetail(row, omitting: appOwned ? shared.union([.appOwned]) : shared)
            )
            folders[tag, default: (place.map { Wording.escaped($0.title) } ?? "Inside apps' own folders", place == nil ? .appOwned : .folder, [])].items.append(item)
        }
        return folders.map { tag, folder in
            let note = folder.kind == .appOwned ? " · the app may need it back" : ""
            return .subgroup(GroupLayout.Subgroup(
                key: GroupLayout.Key(entry: entry, tag: tag),
                kind: folder.kind,
                title: folder.title,
                subtitle: Wording.items(folder.items.count, "folder") + note,
                hint: nil,
                checkable: checkable,
                items: Self.ordered(folder.items)
            ))
        }
    }

    private func ruleBlocks(_ entry: Entry, _ rows: [Row], rules: [String: [Row]], checkable: Bool) -> [GroupLayout.Block] {
        Dictionary(grouping: rows, by: { $0.finding.rule }).map { rule, members in
            let all = rules[rule] ?? members
            let shared = Self.sharedNotes(all)
            let title = Wording.escaped(rule.split(separator: ".").last.map(String.init) ?? rule)
            let noted = ScanReport.Note.allCases.filter(shared.contains).map(CleanWording.note)
            let hint = all.first.flatMap { CleanWording.hint($0.finding.hint) }
            if all.count == 1, let row = members.first {
                return .single(GroupLayout.Single(
                    title: title,
                    subtitle: ([rule, CleanWording.actionText(all)] + noted).joined(separator: " · "),
                    hint: hint,
                    item: GroupLayout.Item(row: row, shown: Wording.tilde(row.finding.path, home: report.home), detail: CleanWording.ownDetail(row, omitting: shared))
                ))
            }
            return .subgroup(GroupLayout.Subgroup(
                key: GroupLayout.Key(entry: entry, tag: Array("r".utf8) + Array(rule.utf8)),
                kind: .rule(rule),
                title: title,
                subtitle: ([rule, Wording.items(members.count, "folder"), CleanWording.actionText(all)] + noted).joined(separator: " · "),
                hint: hint,
                checkable: checkable,
                items: Self.ordered(members.map { row in
                    GroupLayout.Item(row: row, shown: Wording.tilde(row.finding.path, home: report.home), detail: CleanWording.ownDetail(row, omitting: shared))
                })
            ))
        }
    }

    /// The notes every row carries; none for an empty list.
    private static func sharedNotes(_ rows: [Row]) -> Set<ScanReport.Note> {
        guard let first = rows.first else { return [] }
        return rows.dropFirst().reduce(Set(first.finding.notes)) { $0.intersection($1.finding.notes) }
    }

    /// A path's folder subgroup: the first folder under the home folder, a
    /// volume, or the first component elsewhere; and the path shown inside it.
    /// Split on "/" bytes, never on characters: a name that starts with a
    /// combining mark stays its own.
    static func folder(of path: String, home: String) -> (title: String, shown: String) {
        let slash = UInt8(ascii: "/")
        func text(_ parts: ArraySlice<ArraySlice<UInt8>>) -> String {
            String(decoding: Array(parts.joined(separator: [slash])), as: UTF8.self)
        }
        var base = Array(home.utf8)
        if base.last == slash { base.removeLast() }
        let bytes = Array(path.utf8)
        let parts: ArraySlice<ArraySlice<UInt8>>
        let title: String
        if !base.isEmpty, bytes.starts(with: base + [slash]) {
            let rest = ArraySlice(bytes[(base.count + 1)...].split(separator: slash))
            guard rest.count > 1 else { return ("~", Wording.escaped(rest.isEmpty ? path : text(rest))) }
            title = "~/" + text(rest.prefix(1))
            parts = rest.dropFirst()
        } else {
            let all = ArraySlice(bytes.split(separator: slash))
            let depth = all.first.map { Array($0) } == Array("Volumes".utf8) && all.count > 2 ? 2 : 1
            guard all.count > depth else { return ("/" + text(all.prefix(1)), Wording.escaped(path)) }
            title = "/" + text(all.prefix(depth))
            parts = all.dropFirst(depth)
        }
        return (title, Wording.escaped(text(parts)))
    }

    /// Largest first, then by tag: blocks of a group.
    private static func ordered(_ blocks: [GroupLayout.Block]) -> [GroupLayout.Block] {
        blocks.sorted { $0.bytes != $1.bytes ? $0.bytes > $1.bytes : $0.tag.lexicographicallyPrecedes($1.tag) }
    }

    /// Largest first, then by id: items of a subgroup.
    private static func ordered(_ items: [GroupLayout.Item]) -> [GroupLayout.Item] {
        items.sorted { $0.row.finding.bytes != $1.row.finding.bytes ? $0.row.finding.bytes > $1.row.finding.bytes : $0.id < $1.id }
    }
}
