import Foundation

/// The order of the Review window's rows (story "Review window: sort a
/// group's rows by name, size or last change"): one column and its
/// direction, shared by the Groups view and the Items view, and kept across
/// groups and launches (`AppPreferences.reviewSort`).
///
/// One comparator orders the Items view, a subgroup's rows and the
/// subgroups themselves. It orders only what the filter lets through.
public struct ReviewSort: Codable, Hashable, Sendable {
    public enum Column: String, Codable, CaseIterable, Sendable {
        /// The path as shown, in Finder's order.
        case name
        case size
        /// The newest change inside a finding, or an entry's own change.
        case changed
    }

    public let column: Column
    public let ascending: Bool

    public init(column: Column, ascending: Bool) {
        self.column = column
        self.ascending = ascending
    }

    /// Size, largest first: the order both views had before the header, so
    /// nothing moves for a user who never clicks. It is the old "Biggest
    /// First"; the old "By Path" is Name ascending.
    public static let `default` = ReviewSort(column: .size, ascending: false)

    /// A column title clicked: the sorted column reverses; another starts
    /// at its first direction. Size starts largest first, Changed oldest
    /// first (what the user looks for), Name A to Z.
    public func clicking(_ column: Column) -> ReviewSort {
        column == self.column ? ReviewSort(column: column, ascending: !ascending) : ReviewSort(column: column, ascending: column != .size)
    }

    /// What the sort reads of a row, a subgroup or a one-row rule.
    struct Key {
        /// As shown: a row's path, a subgroup's title.
        let name: String
        let bytes: UInt64
        /// RFC 3339 as the CLI writes it, fixed-width UTC, so the text
        /// compares as the time does; nil when there is no date.
        let changed: String?
    }

    /// Whether `a` comes first; nil when they tie. An undated one comes
    /// last in either direction. Ties on the column fall back to size,
    /// largest first, whatever the direction; the caller breaks the rest.
    func precedes(_ a: Key, _ b: Key) -> Bool? {
        switch column {
        case .name:
            let order = a.name.localizedStandardCompare(b.name)
            if order != .orderedSame { return (order == .orderedAscending) == ascending }
        case .size:
            if a.bytes != b.bytes { return (a.bytes < b.bytes) == ascending }
        case .changed:
            switch (a.changed, b.changed) {
            case (nil, nil): break
            case (nil, _?): return false
            case (_?, nil): return true
            case let (x?, y?): if x != y { return (x < y) == ascending }
            }
        }
        if a.bytes != b.bytes { return a.bytes > b.bytes }
        return nil
    }

    /// `values` in this order; `tie` breaks what the keys leave equal (an
    /// id, a subgroup's tag), so the order never depends on the input's.
    func sorted<T>(_ values: [T], key: (T) -> Key, tie: (T, T) -> Bool) -> [T] {
        values.map { ($0, key($0)) }.sorted { a, b in precedes(a.1, b.1) ?? tie(a.0, b.0) }.map(\.0)
    }
}
