import AppKit
import SwiftUI

/// The menu bar icon: a left click toggles the popover, a right click (or a
/// control-click) shows the menu.
///
/// The menu is set on the status item only while it is open: a status item
/// whose `menu` is set ignores its button's action, so the popover would
/// never open (GrammarHelper keeps it set and has no popover).
@MainActor
final class StatusItemController: NSObject, NSMenuDelegate {
    private let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
    private let popover = NSPopover()
    private let menu = NSMenu()

    init(scanNow: @escaping @MainActor () -> Void, openSettings: @escaping @MainActor () -> Void) {
        super.init()
        if let button = item.button {
            button.image = Self.glyph()
            button.setAccessibilityLabel("DeepDriveCleaner")
            button.target = self
            button.action = #selector(clicked(_:))
            button.sendAction(on: [.leftMouseUp, .rightMouseUp])
        }
        popover.behavior = .transient
        popover.contentViewController = NSHostingController(rootView: PopoverView(scanNow: { [weak self] in
            self?.popover.performClose(nil)
            scanNow()
        }))

        menu.addItem(ClosureMenuItem("Scan now", handler: scanNow))
        menu.addItem(ClosureMenuItem("Settings…", key: ",", handler: openSettings))
        #if DEBUG
        menu.addItem(.separator())
        menu.addItem(DebugMenu.item())
        #endif
        menu.addItem(.separator())
        menu.addItem(ClosureMenuItem("Quit DeepDriveCleaner", key: "q") { NSApp.terminate(nil) })
        menu.delegate = self
    }

    @objc private func clicked(_ sender: NSStatusBarButton) {
        let event = NSApp.currentEvent
        if event?.type == .rightMouseUp || event?.modifierFlags.contains(.control) == true {
            popover.performClose(nil)
            item.menu = menu
            sender.performClick(nil)
        } else if popover.isShown {
            popover.performClose(nil)
        } else {
            NSApp.activate()
            popover.show(relativeTo: sender.bounds, of: sender, preferredEdge: .minY)
        }
    }

    func menuDidClose(_ menu: NSMenu) {
        item.menu = nil
    }

    /// The drive glyph as an 18 pt template image, tinted by the menu bar.
    private static func glyph() -> NSImage {
        let image = NSImage(size: NSSize(width: 18, height: 18), flipped: false) { rect in
            guard let context = NSGraphicsContext.current?.cgContext else { return false }
            DriveGlyph.draw(in: context, rect: rect, color: NSColor.black.cgColor)
            return true
        }
        image.isTemplate = true
        image.accessibilityDescription = "DeepDriveCleaner"
        return image
    }
}
