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

        let scanItem = ClosureMenuItem("Scan now") { Task { await model.scanNow() } }
        let pauseItem = ClosureMenuItem("Pause scans") { model.setPaused(!model.paused) }
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
        NSApp.activate()
        popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
        // The popover's window focuses its first button (the gear) when it
        // opens; nothing should look selected until the user tabs.
        popover.contentViewController?.view.window?.makeFirstResponder(nil)
    }

    func menuWillOpen(_ menu: NSMenu) {
        pauseItem?.title = model.paused ? "Resume scans" : "Pause scans"
        scanItem?.isEnabled = model.canScan
        scanItem?.title = model.paused ? "Scan now (paused)" : "Scan now"
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
                guard let self else { return }
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
