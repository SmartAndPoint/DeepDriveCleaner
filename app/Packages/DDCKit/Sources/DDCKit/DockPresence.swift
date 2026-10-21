/// Whether the app shows in the Dock and in Cmd-Tab (menu bar ADR §2, amended
/// 2026-10-20 for 0.2.1): while any of its titled windows is open (Review,
/// Settings, the About panel) it is a regular app, so a window behind other
/// apps can be found again; after the last one closes it is menu-bar only and
/// hands the focus back. The popover and the status item never count. A pure
/// decision over window identities; the app applies its effects.
public struct DockPresence<ID: Hashable & Sendable>: Sendable {
    public enum Effect: Sendable, Equatable {
        /// In the Dock and Cmd-Tab, with the main menu.
        case regular
        /// Menu bar only.
        case accessory
        /// Give the focus back to the app that had it.
        case yieldActivation
    }

    public private(set) var open: Set<ID> = []

    public var isRegular: Bool { !open.isEmpty }

    public init() {}

    /// A titled window is about to show; only the first one changes anything.
    public mutating func willShow(_ id: ID) -> [Effect] {
        let wasRegular = isRegular
        open.insert(id)
        return wasRegular ? [] : [.regular]
    }

    /// A window closed. When its close leads to the popover, the popover
    /// activates the app itself, and a yield just before would be refused.
    public mutating func didClose(_ id: ID, returningToPopover: Bool = false) -> [Effect] {
        guard open.remove(id) != nil, open.isEmpty else { return [] }
        return returningToPopover ? [.accessory] : [.accessory, .yieldActivation]
    }
}
