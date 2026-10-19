import Foundation

/// An action as `clean --items` and the clean report name it (`ActionKind`
/// in `crates/ddc-core/src/plan.rs`).
public enum ListedAction: String, Codable, Sendable, CaseIterable {
    case delete, trash, native
    case nativeOnly = "native-only"

    /// `plan::action_for` as the scan report exposes it: the report's
    /// `removal` is already the effective method (a `review` finding's
    /// `delete` reads `trash`). Nil: nothing ddc may do with it.
    ///
    /// `switchable` is the rule's flag from `rules --json`: it is false for a
    /// runtime ddc has no command for (`NativeOnly(&[])`), which the report
    /// still shows as `native-only`. `permanent` deletes a `review` item
    /// instead of moving it to the Trash.
    public static func derive(_ finding: ScanReport.Finding, switchable: Bool?, permanent: Bool) -> ListedAction? {
        if finding.class == .userData || finding.ruleClass == .userData { return nil }
        switch finding.removal {
        case .none: return nil
        case .nativeOnly: return switchable == true ? .nativeOnly : nil
        case .native: return .native
        // The report never shows this pair (it rewrites it to `trash`); mirrored all the same.
        case .delete: return finding.class == .review && !permanent ? .trash : .delete
        case .trash: return permanent && finding.class == .review ? .delete : .trash
        }
    }
}

/// One confirmed item of `clean --items` (`ListedItem` in `plan.rs`). The
/// CLI refuses unknown keys, so this has exactly its fields.
public struct ListedItem: Encodable, Sendable, Equatable {
    /// Exactly as the scan report wrote it: the CLI compares it byte for byte.
    public let path: String
    public let dev: UInt64
    public let ino: UInt64
    public let rule: String
    public let `class`: ScanReport.RiskClass
    public let action: ListedAction
    /// Required on a `review` item listed as `delete`, allowed nowhere else.
    public let permanent: Bool

    private enum CodingKeys: String, CodingKey {
        case path, dev, ino, rule, `class`, action, permanent
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(path, forKey: .path)
        try c.encode(dev, forKey: .dev)
        try c.encode(ino, forKey: .ino)
        try c.encode(rule, forKey: .rule)
        try c.encode(`class`, forKey: .class)
        try c.encode(action, forKey: .action)
        // serde's `skip_serializing_if = "Not::not"`.
        if permanent { try c.encode(true, forKey: .permanent) }
    }
}

/// The input of `clean --items <file>`: the items the user confirmed in the
/// sheet, in its order (ADR "Menu bar app: Swift shell over the ddc CLI", §6 item 1).
public struct ItemList: Encodable, Sendable, Equatable {
    /// `LISTED_SCHEMA_VERSION` of the CLI this app embeds.
    public static let schemaVersion = 1

    public let items: [ListedItem]

    public init(items: [ListedItem]) {
        self.items = items
    }

    private enum CodingKeys: String, CodingKey {
        case schemaVersion = "schema_version"
        case items
    }

    public func encode(to encoder: any Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(Self.schemaVersion, forKey: .schemaVersion)
        try c.encode(items, forKey: .items)
    }

    /// The file's bytes. Slashes stay unescaped, so a path reads as the CLI
    /// wrote it; keys are sorted, so one list always gives the same bytes.
    public func encoded() throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.withoutEscapingSlashes, .sortedKeys]
        return try encoder.encode(self)
    }
}
