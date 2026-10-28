import AppKit
import DDCKit
import SwiftUI

/// The Whole disk window, opened from the popover's "Where Does the Rest
/// Go?" (story "Whole disk in the app"). Closing it never stops a running
/// whole-disk scan: the run lives in `DiskModel`.
@MainActor
final class DiskWindowController {
    private let presence: DockPresenceController
    private let model: DiskModel
    private let popover: PopoverModel
    private var window: NSWindow?

    init(presence: DockPresenceController, model: DiskModel, popover: PopoverModel) {
        self.presence = presence
        self.model = model
        self.popover = popover
    }

    func show() {
        // A window shown again does not run the view's `.task`: read a
        // report written meanwhile, and today's date for "Measured today".
        if window != nil { Task { await model.load() } }
        if window == nil {
            let hosting = NSHostingController(rootView: DiskView(model: model, popover: popover).frame(minWidth: 640, minHeight: 480))
            let window = NSWindow(contentViewController: hosting)
            window.title = "Whole Disk"
            window.styleMask = [.titled, .closable, .miniaturizable, .resizable]
            window.setContentSize(NSSize(width: 820, height: 680))
            window.isReleasedWhenClosed = false
            window.setFrameAutosaveName("WholeDiskWindow")
            if !window.setFrameUsingName("WholeDiskWindow") { window.center() }
            self.window = window
        }
        if let window { presence.show(window) }
    }
}
