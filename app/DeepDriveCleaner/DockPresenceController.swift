import AppKit
import DDCKit

/// Applies `DockPresence`: the app is in the Dock and Cmd-Tab while one of its
/// titled windows is open, and menu-bar only after the last one closes, with
/// the focus handed back (menu bar ADR §2, amended 2026-10-20 for 0.2.1).
@MainActor
final class DockPresenceController {
    private var presence = DockPresence<ObjectIdentifier>()
    private var closeObservers: [ObjectIdentifier: NSObjectProtocol] = [:]
    private var activationObserver: NSObjectProtocol?
    /// The last other app that was active: the focus goes back to it.
    private var previous: NSRunningApplication?
    /// The window whose close leads to the popover, while it closes.
    private var closingToPopover: ObjectIdentifier?

    var isRegular: Bool { presence.isRegular }

    init() {
        let own = ProcessInfo.processInfo.processIdentifier
        if let front = NSWorkspace.shared.frontmostApplication, front.processIdentifier != own {
            previous = front
        }
        activationObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didActivateApplicationNotification,
            object: nil,
            queue: .main
        ) { [weak self] note in
            guard let app = note.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication,
                  app.processIdentifier != own else { return }
            let pid = app.processIdentifier
            MainActor.assumeIsolated { self?.previous = NSRunningApplication(processIdentifier: pid) }
        }
    }

    /// Shows one of the app's windows: in the Dock first, then the window in
    /// front, then the app active, so the main menu appears with it.
    func show(_ window: NSWindow) {
        track(window)
        window.makeKeyAndOrderFront(nil)
        NSApp.activate()
    }

    /// The About panel counts as a window too, or closing the Review window
    /// would leave it behind with no Dock icon.
    func showAbout() {
        NSApp.orderFrontStandardAboutPanel(nil)
        NSApp.activate()
        // Not a sheet: the confirmation sheet is visible and titled too.
        let panel = NSApp.windows.first {
            $0 is NSPanel && $0.isVisible && $0.styleMask.contains(.titled) && !$0.isSheet && $0.sheetParent == nil
                && closeObservers[ObjectIdentifier($0)] == nil
        }
        if let panel { track(panel) }
    }

    /// The Review window's Close button: the popover opens next and activates
    /// the app itself, so this close does not hand the focus away.
    func closeReturningToPopover(_ window: NSWindow) {
        closingToPopover = ObjectIdentifier(window)
        window.performClose(nil)
        closingToPopover = nil
    }

    /// A click on the Dock icon: the open windows come back, minimized ones too.
    func frontAll() {
        let windows = NSApp.windows.filter { closeObservers[ObjectIdentifier($0)] != nil }
        for window in windows where window.isMiniaturized {
            window.deminiaturize(nil)
        }
        windows.first?.makeKeyAndOrderFront(nil)
        NSApp.activate()
    }

    private func track(_ window: NSWindow) {
        let id = ObjectIdentifier(window)
        apply(presence.willShow(id))
        guard closeObservers[id] == nil else { return }
        closeObservers[id] = NotificationCenter.default.addObserver(
            forName: NSWindow.willCloseNotification,
            object: window,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.closed(id) }
        }
    }

    private func closed(_ id: ObjectIdentifier) {
        if let observer = closeObservers.removeValue(forKey: id) {
            NotificationCenter.default.removeObserver(observer)
        }
        apply(presence.didClose(id, returningToPopover: closingToPopover == id))
    }

    private func apply(_ effects: [DockPresence<ObjectIdentifier>.Effect]) {
        for effect in effects {
            switch effect {
            case .regular:
                NSApp.setActivationPolicy(.regular)
                AppLog.write("in the Dock while a window is open")
            case .accessory:
                NSApp.setActivationPolicy(.accessory)
                AppLog.write("menu bar only again")
            case .yieldActivation:
                guard let previous, !previous.isTerminated else {
                    NSApp.deactivate()
                    continue
                }
                NSApp.yieldActivation(to: previous)
                previous.activate(from: .current, options: [])
            }
        }
    }
}
