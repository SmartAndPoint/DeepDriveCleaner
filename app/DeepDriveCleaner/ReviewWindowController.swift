import AppKit
import DDCKit
import SwiftUI

/// The Review and clean window (artboards 4 to 6). Closing it never stops a
/// clean: the clean lives in `ReviewModel`, and the sheet comes back with
/// its state when the window opens again.
@MainActor
final class ReviewWindowController {
    private let presence: DockPresenceController
    private var window: NSWindow?
    private var keyObserver: NSObjectProtocol?
    private var closeObserver: NSObjectProtocol?
    /// The Close button returns to the popover the user came from.
    var returnToPopover: (() -> Void)?
    /// The Whole disk detail; `AppDelegate` sets it at launch, before
    /// anything can ask for the window.
    var wholeDisk: (() -> DiskView)?

    init(presence: DockPresenceController) {
        self.presence = presence
    }

    /// Shows the window for `model`; the model's `present`.
    func show(_ model: ReviewModel) -> Bool {
        if window == nil {
            guard let wholeDisk else { return false }
            let hosting = NSHostingController(rootView: ReviewView(model: model, close: { [weak self] in
                guard let self else { return }
                if let window = self.window { self.presence.closeReturningToPopover(window) }
                self.returnToPopover?()
            }, wholeDisk: wholeDisk))
            let window = NSWindow(contentViewController: hosting)
            window.title = "Review and Clean"
            window.styleMask = [.titled, .closable, .miniaturizable, .resizable]
            window.setContentSize(NSSize(width: 1040, height: 700))
            window.contentMinSize = NSSize(width: 820, height: 520)
            window.isReleasedWhenClosed = false
            window.setFrameAutosaveName("ReviewWindow")
            if !window.setFrameUsingName("ReviewWindow") { window.center() }
            // A type switched off in Settings applies when the user comes back.
            keyObserver = NotificationCenter.default.addObserver(
                forName: NSWindow.didBecomeKeyNotification,
                object: window,
                queue: .main
            ) { _ in
                MainActor.assumeIsolated { model.refreshSettings() }
            }
            // Folded subgroups open again next time.
            closeObserver = NotificationCenter.default.addObserver(
                forName: NSWindow.willCloseNotification,
                object: window,
                queue: .main
            ) { _ in
                MainActor.assumeIsolated { model.windowClosed() }
            }
            self.window = window
        }
        if let window { presence.show(window) }
        return window != nil
    }

    /// Edit → Find (⌘F): focuses the filter, only while this window is key.
    func find() {
        guard window?.isKeyWindow == true else { return }
        NotificationCenter.default.post(name: .findInReview, object: nil)
    }
}

extension Notification.Name {
    /// Edit → Find in the Review window; `ReviewView` focuses its filter.
    static let findInReview = Notification.Name("DeepDriveCleaner.findInReview")
}
