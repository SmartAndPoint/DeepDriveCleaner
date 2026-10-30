/// What the confirmation sheet lists and the clean sends (artboard 5).
///
/// It is captured when the sheet opens: a selection changed behind the sheet,
/// or a report reloaded meanwhile, changes nothing in it. Only the per-item
/// Delete permanently changes it.
public struct Confirmation: Sendable, Equatable {
    public struct Item: Identifiable, Sendable, Equatable {
        /// The row of `ReviewItems`.
        public let id: Int
        public let finding: ScanReport.Finding
        /// The action when not deleted permanently.
        public let action: ListedAction
        public internal(set) var permanent: Bool
        /// Its row's class: a coding agent's session counts under Review
        /// with its folder, though it is listed as `user-data`.
        public let riskClass: ScanReport.RiskClass
        /// A coding agent's session (story "Review window: pick a coding
        /// agent's sessions one by one").
        public let isSession: Bool

        /// A `review` item the Trash would take.
        public var canBePermanent: Bool { finding.class == .review && action == .trash }
        /// What the clean does with it.
        public var effectiveAction: ListedAction { permanent && canBePermanent ? .delete : action }
    }

    public struct ClassRow: Sendable, Equatable {
        public let riskClass: ScanReport.RiskClass
        public let count: Int
        public let bytes: UInt64
    }

    /// In the sheet's order: the user's own files first, then by class,
    /// then biggest first.
    public private(set) var items: [Item]
    public let home: String
    /// Selected findings inside a picked folder: they go to the Trash with it
    /// and are not sent on their own.
    public let included: Int

    /// The sheet's class order: the user's own files first.
    public static let order: [ScanReport.RiskClass] = [.userData, .safe, .rebuildable, .review]

    /// The selectable rows of `selection`; a `review` item that the Trash
    /// would take starts permanent when the app preference says so.
    public init(_ snapshot: ReviewItems, selection: Set<Int>, deletePermanently: Bool) {
        let order = Self.order
        let covered = snapshot.covered(by: selection)
        included = covered.count
        items = snapshot.rows
            .filter { selection.contains($0.id) && $0.selectable && !covered.contains($0.id) }
            .sorted { a, b in
                let (x, y) = (order.firstIndex(of: a.riskClass) ?? 0, order.firstIndex(of: b.riskClass) ?? 0)
                if x != y { return x < y }
                return a.finding.bytes != b.finding.bytes ? a.finding.bytes > b.finding.bytes : a.id < b.id
            }
            .compactMap { row in
                guard let action = row.action else { return nil }
                return Item(
                    id: row.id, finding: row.finding, action: action, permanent: deletePermanently && row.canBePermanent,
                    riskClass: row.riskClass, isSession: row.userEntry?.isSession == true
                )
            }
        home = snapshot.report.home
    }

    /// Delete permanently on one item; no other item changes.
    public mutating func togglePermanent(_ id: Int) {
        guard let index = items.firstIndex(where: { $0.id == id }), items[index].canBePermanent else { return }
        items[index].permanent.toggle()
    }

    /// "Selected": what the scan measured, as the Review window and the class rows name it.
    public var bytes: UInt64 { items.reduce(0) { $0 + $1.finding.bytes } }
    /// What the clean frees. A `native-only` item frees space inside its
    /// runtime's disk image, which no total counts (safety ADR §5), as the
    /// terminal's "frees ~" leaves it out.
    public var reclaimable: UInt64 { reclaimable(excluding: []) }
    /// `reclaimable` without the items of `ids`: the header leaves out the
    /// items the dry run predicts to be skipped.
    public func reclaimable(excluding ids: Set<Int>) -> UInt64 {
        counting.reduce(0) { ids.contains($1.id) ? $0 : $0 + $1.finding.reclaimableBytes }
    }
    /// `bytes` without `native-only` items: the bar's total until the CLI names its own.
    public var counted: UInt64 { counting.reduce(0) { $0 + $1.finding.bytes } }
    /// The `native-only` items' bytes by runtime, in the sheet's order.
    public var notCounted: [(runtime: String, bytes: UInt64)] {
        var parts: [(runtime: String, bytes: UInt64)] = []
        for item in items where item.effectiveAction == .nativeOnly {
            let runtime = CleanWording.runtime(rule: item.finding.rule)
            if let index = parts.firstIndex(where: { $0.runtime == runtime }) {
                parts[index].bytes += item.finding.bytes
            } else {
                parts.append((runtime, item.finding.bytes))
            }
        }
        return parts
    }

    private var counting: [Item] { items.filter { $0.effectiveAction != .nativeOnly } }
    /// Items the Trash takes: the space stays used until it is emptied.
    public var trashCount: Int { items.filter { $0.effectiveAction == .trash }.count }
    /// Every item goes to the Trash, by its action as the sheet now sets it:
    /// the sheet says "Move", not "Clean".
    public var movesOnlyToTrash: Bool { !items.isEmpty && trashCount == items.count }

    /// "Safe · 1,919 items · 76.5 GiB", for every class that has items, by
    /// the row's class: a session counts under Review.
    public var classRows: [ClassRow] {
        Self.order.compactMap { riskClass in
            let of = items.filter { $0.riskClass == riskClass }
            return of.isEmpty ? nil : ClassRow(riskClass: riskClass, count: of.count, bytes: of.reduce(0) { $0 + $1.finding.bytes })
        }
    }

    /// The `--items` payload: these items in this order, each checked again
    /// against `snapshot`. One that is no longer selectable there (protected
    /// since, or never selectable) is left out, never sent.
    public func itemList(checkedAgainst snapshot: ReviewItems) -> ItemList {
        ItemList(items: listed(checkedAgainst: snapshot).map(\.item))
    }

    /// The payload's items with the row each came from, in the same order.
    public func listed(checkedAgainst snapshot: ReviewItems) -> [(id: Int, item: ListedItem)] {
        items.compactMap { item in
            guard let row = snapshot.row(item.id), row.selectable, row.finding == item.finding, let action = row.action else { return nil }
            let permanent = item.permanent && row.canBePermanent
            let effective = permanent ? ListedAction.delete : action
            return (item.id, ListedItem(
                path: item.finding.path,
                dev: item.finding.dev,
                ino: item.finding.ino,
                rule: item.finding.rule,
                class: item.finding.class,
                action: effective,
                permanent: item.finding.class == .review && effective == .delete
            ))
        }
    }
}
