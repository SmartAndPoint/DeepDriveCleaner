import AppKit

/// A menu item that runs a closure, so menus need no selector per action.
final class ClosureMenuItem: NSMenuItem {
    private let handler: @MainActor () -> Void

    init(_ title: String, key: String = "", handler: @escaping @MainActor () -> Void) {
        self.handler = handler
        super.init(title: title, action: #selector(fire), keyEquivalent: key)
        target = self
    }

    @available(*, unavailable)
    required init(coder: NSCoder) {
        fatalError("not used from a nib")
    }

    // AppKit sends menu actions on the main thread.
    @MainActor @objc private func fire() {
        handler()
    }
}
