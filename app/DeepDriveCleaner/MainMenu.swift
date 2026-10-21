import AppKit

/// The main menu, shown while a window keeps the app in the Dock (menu bar ADR
/// §2, amended 2026-10-20 for 0.2.1): ⌘Q, ⌘W and the Edit shortcuts of the
/// search field go through it.
@MainActor
enum MainMenu {
    static func make(about: @escaping @MainActor () -> Void, settings: @escaping @MainActor () -> Void) -> NSMenu {
        let main = NSMenu()

        let app = NSMenu(title: "DeepDriveCleaner")
        app.addItem(ClosureMenuItem("About DeepDriveCleaner", handler: about))
        app.addItem(.separator())
        app.addItem(ClosureMenuItem("Settings…", key: ",", handler: settings))
        app.addItem(.separator())
        app.addItem(withTitle: "Hide DeepDriveCleaner", action: #selector(NSApplication.hide(_:)), keyEquivalent: "h")
        app.addItem(withTitle: "Quit DeepDriveCleaner", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        main.addItem(submenu(app))

        let edit = NSMenu(title: "Edit")
        edit.addItem(withTitle: "Undo", action: Selector(("undo:")), keyEquivalent: "z")
        edit.addItem(withTitle: "Redo", action: Selector(("redo:")), keyEquivalent: "z").keyEquivalentModifierMask = [.command, .shift]
        edit.addItem(.separator())
        edit.addItem(withTitle: "Cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        edit.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        edit.addItem(withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        edit.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        main.addItem(submenu(edit))

        let window = NSMenu(title: "Window")
        window.addItem(withTitle: "Minimize", action: #selector(NSWindow.performMiniaturize(_:)), keyEquivalent: "m")
        window.addItem(withTitle: "Close", action: #selector(NSWindow.performClose(_:)), keyEquivalent: "w")
        main.addItem(submenu(window))
        NSApp.windowsMenu = window

        return main
    }

    private static func submenu(_ menu: NSMenu) -> NSMenuItem {
        let item = NSMenuItem(title: menu.title, action: nil, keyEquivalent: "")
        item.submenu = menu
        return item
    }
}
