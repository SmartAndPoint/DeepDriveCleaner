import AppKit
import DDCKit
import SwiftUI

/// The Settings window (artboard 10): toolbar tabs General, Folders,
/// Schedule, Automation, Types and Permissions. An `NSTabViewController`
/// with `.toolbar` style, since a SwiftUI `TabView` in an `NSWindow` draws
/// segments, not toolbar tabs. The window title is the pane's name, and the
/// window takes each pane's height.
@MainActor
final class SettingsWindowController {
    enum Pane: Int, CaseIterable {
        case general, folders, schedule, automation, types, permissions

        /// The pane the popover's Next scan line leads to.
        init(_ destination: SettingsDestination) {
            switch destination {
            case .schedule: self = .schedule
            case .permissions: self = .permissions
            }
        }

        var title: String {
            switch self {
            case .general: "General"
            case .folders: "Folders"
            case .schedule: "Schedule"
            case .automation: "Automation"
            case .types: "Types"
            case .permissions: "Permissions"
            }
        }

        var symbol: String {
            switch self {
            case .general: "gearshape"
            case .folders: "folder"
            case .schedule: "clock"
            case .automation: "bell.badge"
            case .types: "square.grid.2x2"
            case .permissions: "lock.shield"
            }
        }
    }

    private let presence: DockPresenceController
    private let model: SettingsModel
    private let privacy: PrivacySettings
    private let loginItem = LoginItem()
    private let updates: UpdaterSettings
    private let notifications = NotificationPermission()
    private var window: NSWindow?
    private var tabs: SettingsTabViewController?
    private var activeObserver: NSObjectProtocol?

    init(presence: DockPresenceController, model: SettingsModel, updates: UpdaterSettings, privacy: PrivacySettings) {
        self.presence = presence
        self.model = model
        self.updates = updates
        self.privacy = privacy
    }

    /// Opens the window on `pane`, or on the pane it shows when none is given.
    func show(pane: Pane? = nil) {
        if window == nil { makeWindow() }
        if let pane { tabs?.selectedTabViewItemIndex = pane.rawValue }
        refresh()
        if let window { presence.show(window) }
    }

    private func makeWindow() {
        let tabs = SettingsTabViewController()
        tabs.tabStyle = .toolbar
        for pane in Pane.allCases {
            let hosting = NSHostingController(rootView: view(for: pane))
            // The window takes the pane's height, and follows it as rows come and go.
            hosting.sizingOptions = .preferredContentSize
            let item = NSTabViewItem(viewController: hosting)
            item.label = pane.title
            item.image = NSImage(systemSymbolName: pane.symbol, accessibilityDescription: pane.title)
            tabs.addTabViewItem(item)
        }
        let window = NSWindow(contentViewController: tabs)
        window.styleMask = [.titled, .closable]
        window.toolbarStyle = .preference
        window.isReleasedWhenClosed = false
        window.setFrameAutosaveName("SettingsWindow")
        if !window.setFrameUsingName("SettingsWindow") { window.center() }
        self.window = window
        self.tabs = tabs
        // The user may change Login Items, notifications or Full Disk Access
        // in System Settings while the window is open.
        activeObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.didBecomeActiveNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.refresh() }
        }
    }

    private func refresh() {
        guard window != nil else { return }
        model.refresh()
        loginItem.refresh()
        updates.refresh()
        Task { await notifications.refresh() }
    }

    private func view(for pane: Pane) -> AnyView {
        switch pane {
        case .general: AnyView(GeneralSettingsView(model: model, loginItem: loginItem, updates: updates))
        case .folders: AnyView(FoldersSettingsView(model: model, privacy: privacy))
        case .schedule: AnyView(ScheduleSettingsView(model: model))
        case .automation: AnyView(AutomationSettingsView(model: model))
        case .types: AnyView(TypesSettingsView(model: model))
        case .permissions: AnyView(PermissionsSettingsView(model: model, notifications: notifications, loginItem: loginItem, privacy: privacy))
        }
    }
}

/// Puts the pane's name in the title bar and fits the window to the pane,
/// its top edge kept where it is.
final class SettingsTabViewController: NSTabViewController {
    override func tabView(_ tabView: NSTabView, didSelect tabViewItem: NSTabViewItem?) {
        super.tabView(tabView, didSelect: tabViewItem)
        guard let tabViewItem else { return }
        view.window?.title = tabViewItem.label
        fit(tabViewItem.viewController)
    }

    override func preferredContentSizeDidChange(for viewController: NSViewController) {
        super.preferredContentSizeDidChange(for: viewController)
        guard let selected = tabViewItems[safe: selectedTabViewItemIndex], selected.viewController === viewController else { return }
        fit(viewController)
    }

    override func viewDidAppear() {
        super.viewDidAppear()
        if let selected = tabViewItems[safe: selectedTabViewItemIndex] {
            view.window?.title = selected.label
            fit(selected.viewController)
        }
    }

    private func fit(_ controller: NSViewController?) {
        guard let window = view.window, let size = controller?.preferredContentSize, size.width > 0, size.height > 0 else { return }
        let content = window.contentRect(forFrameRect: window.frame)
        guard content.size != size else { return }
        var target = window.frameRect(forContentRect: NSRect(x: content.minX, y: content.maxY - size.height, width: size.width, height: size.height))
        // Never past the bottom of the screen: keep the whole window visible.
        if let visible = window.screen?.visibleFrame, target.minY < visible.minY {
            target.origin.y = visible.minY
        }
        window.setFrame(target, display: true, animate: window.isVisible && !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion)
    }
}

private extension Array {
    subscript(safe index: Int) -> Element? {
        indices.contains(index) ? self[index] : nil
    }
}

/// The System Settings panes the Settings window and the popover open.
@MainActor
struct PrivacySettings {
    /// Privacy & Security → Full Disk Access.
    let openFullDiskAccess: @MainActor () -> Void
    /// Privacy & Security → Files and Folders, where a folder the user said no to is allowed again.
    let openFilesAndFolders: @MainActor () -> Void
}
