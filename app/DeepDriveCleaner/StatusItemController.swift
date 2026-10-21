import AppKit
import DDCKit
import SwiftUI

/// The menu bar icon: a left click toggles the popover, a right click (or a
/// control-click) shows the menu. The icon follows the model's `iconState`.
///
/// The menu is set on the status item only while it is open: a status item
/// whose `menu` is set ignores its button's action, so the popover would
/// never open (GrammarHelper keeps it set and has no popover).
@MainActor
final class StatusItemController: NSObject, NSMenuDelegate {
    private let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
    private let popover = NSPopover()
    private let menu = NSMenu()
    private let model: PopoverModel
    private var icon: IconState?
    private var arcTimer: Timer?
    private var arcAngle: CGFloat = 0
    private var refreshTimer: Timer?
    private var scanItem: NSMenuItem?
    private var pauseItem: NSMenuItem?
    /// Set by a click that found the app inactive: its activation, which
    /// arrives after `show`, makes the popover key once more.
    private var keyOnActivation = false
    private var activationObserver: NSObjectProtocol?

    init(model: PopoverModel, openSettings: @escaping @MainActor () -> Void, openPrivacy: @escaping @MainActor () -> Void) {
        self.model = model
        super.init()
        if let button = item.button {
            button.target = self
            button.action = #selector(clicked(_:))
            button.sendAction(on: [.leftMouseUp, .rightMouseUp])
        }
        popover.behavior = .transient
        let view = PopoverView(
            model: model,
            openSettings: { [weak self] in
                self?.popover.performClose(nil)
                openSettings()
            },
            openPrivacy: { [weak self] in
                self?.popover.performClose(nil)
                openPrivacy()
            }
        )
        let hosting = NSHostingController(rootView: view)
        // The popover takes the height of the state it shows.
        hosting.sizingOptions = .preferredContentSize
        popover.contentViewController = hosting

        let scanItem = ClosureMenuItem("Scan Now") { Task { await model.scanNow() } }
        let pauseItem = ClosureMenuItem("Pause Scans") { model.setPaused(!model.paused) }
        menu.addItem(scanItem)
        menu.addItem(pauseItem)
        menu.addItem(ClosureMenuItem("Settings…", key: ",", handler: openSettings))
        self.scanItem = scanItem
        self.pauseItem = pauseItem
        // Scan now is off while paused; the titles follow the model in `menuWillOpen`.
        menu.autoenablesItems = false
        #if DEBUG
        menu.addItem(.separator())
        menu.addItem(DebugMenu.item(model: model))
        #endif
        menu.addItem(.separator())
        menu.addItem(ClosureMenuItem("Quit DeepDriveCleaner", key: "q") { NSApp.terminate(nil) })
        menu.delegate = self

        followIcon()
        // A scheduled scan or a terminal `ddc scan` may write a new report, and
        // "2 h ago" ages: look once a minute.
        let timer = Timer(timeInterval: 60, repeats: true) { _ in
            Task { @MainActor in await model.refresh() }
        }
        RunLoop.main.add(timer, forMode: .common)
        refreshTimer = timer

        activationObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.didBecomeActiveNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, self.keyOnActivation else { return }
                self.keyOnActivation = false
                // A window the activation brought up keeps the key.
                let popoverWindow = self.popover.contentViewController?.view.window
                if let key = NSApp.keyWindow, key !== popoverWindow {
                    AppLog.write("popover on activation: \(key.title.isEmpty ? "another window" : key.title) kept the key")
                    return
                }
                self.keyPopover("on activation")
            }
        }
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
            showPopover()
        }
    }

    /// Opens the popover under the icon; also where the Review window's Close returns.
    func showPopover() {
        guard let button = item.button, !popover.isShown else { return }
        Task { await model.refresh() }
        keyOnActivation = !NSApp.isActive
        NSApp.activate()
        popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
        keyPopover("shown")
    }

    /// Makes the popover's window key, so it draws active rather than as a
    /// window in the background, then clears the first responder: becoming key
    /// focuses its first button (the gear), and nothing should look selected
    /// until the user tabs. Activation is cooperative since macOS 14 and can
    /// arrive after `show`, so this runs again when it does.
    private func keyPopover(_ cause: String) {
        guard popover.isShown, let window = popover.contentViewController?.view.window else { return }
        window.makeKey()
        window.makeFirstResponder(nil)
        AppLog.write("popover \(cause): app \(NSApp.isActive ? "active" : "inactive"), window \(window.isKeyWindow ? "key" : "not key")")
    }

    func menuWillOpen(_ menu: NSMenu) {
        pauseItem?.title = model.paused ? "Resume Scans" : "Pause Scans"
        scanItem?.isEnabled = model.canScan
        scanItem?.title = model.paused ? "Scan Now (Paused)" : "Scan Now"
    }

    func menuDidClose(_ menu: NSMenu) {
        item.menu = nil
    }

    // MARK: - The icon

    /// Applies the model's icon state, and again whenever it changes.
    private func followIcon() {
        let state = withObservationTracking {
            model.iconState
        } onChange: { [weak self] in
            Task { @MainActor in self?.followIcon() }
        }
        guard state != icon else { return }
        icon = state
        draw()
        if state.inner == .arc { startArc() } else { stopArc() }
    }

    private func draw() {
        guard let icon, let button = item.button else { return }
        button.image = StatusIcon.image(icon, arcAngle: arcAngle)
        button.setAccessibilityLabel(icon.accessibility)
        if let label = icon.label {
            button.imagePosition = .imageLeading
            button.attributedTitle = NSAttributedString(
                string: label,
                attributes: [.font: NSFont.monospacedDigitSystemFont(ofSize: 12, weight: .semibold)]
            )
        } else {
            button.imagePosition = .imageOnly
            button.title = ""
        }
    }

    /// The arc turns only while the CLI runs; in `.common` mode so it keeps
    /// turning while a menu is open.
    private func startArc() {
        guard arcTimer == nil else { return }
        let timer = Timer(timeInterval: 0.1, repeats: true) { [weak self] _ in
            Task { @MainActor in
                // With Reduce Motion the arc stands still; checked on every
                // tick, so switching it on mid-scan takes effect at once.
                guard let self, !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion else { return }
                self.arcAngle -= .pi / 6
                self.draw()
            }
        }
        RunLoop.main.add(timer, forMode: .common)
        arcTimer = timer
    }

    private func stopArc() {
        arcTimer?.invalidate()
        arcTimer = nil
        arcAngle = 0
    }
}
