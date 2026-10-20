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
                return Item(id: row.id, finding: row.finding, action: action, permanent: deletePermanently && row.canBePermanent)
            }
        home = snapshot.report.home
    }

    /// Delete permanently on one item; no other item changes.
    public mutating func togglePermanent(_ id: Int) {
        guard let index = items.firstIndex(where: { $0.id == id }), items[index].canBePermanent else { return }
        items[index].permanent.toggle()
    }

    public var bytes: UInt64 { items.reduce(0) { $0 + $1.finding.bytes } }
    public var reclaimable: UInt64 { items.reduce(0) { $0 + $1.finding.reclaimableBytes } }
    /// Items the Trash takes: the space stays used until it is emptied.
    public var trashCount: Int { items.filter { $0.effectiveAction == .trash }.count }

    /// "Safe · 1,919 items · 76.5 GiB", for every class that has items.
    public var classRows: [ClassRow] {
        Self.order.compactMap { riskClass in
            let of = items.filter { $0.finding.class == riskClass }
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
