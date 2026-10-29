import AppKit
import DDCKit
import UserNotifications

/// Feeds AppKit's launch, reopen and activate events and notification responses
/// to `LaunchMode`, and applies what it decides (ADR §5).
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, UNUserNotificationCenterDelegate {
    /// The per-user TCC database: it opens only with Full Disk Access, which is
    /// how the app and the CLI both tell. Built here only, never in DDCKit, so
    /// no test ever touches it.
    static let tccDatabase = FileManager.default.homeDirectoryForCurrentUser
        .appending(path: "Library/Application Support/com.apple.TCC/TCC.db")

    private var launchMode = LaunchMode()
    private var statusItem: StatusItemController?
    /// In the Dock while a window is open (menu bar ADR §2, amended for 0.2.1).
    private let presence = DockPresenceController()
    /// Made once the popover model exists: Settings shares its state.
    private var settings: SettingsWindowController?
    private lazy var reviewWindow = ReviewWindowController(presence: presence)
    private var review: ReviewModel?
    private var model: PopoverModel?
    /// The Whole disk view's one model: its window and the Review detail
    /// share its run. Never given to `ScheduledWork`, and never stopped on
    /// quit: the CLI saves its report before it prints (ADR §1, amended
    /// 2026-10-28).
    private var disk: DiskModel?
    private var diskWindow: DiskWindowController?
    /// The hourly tick's work and the notification's answers.
    private var work: ScheduledWork?
    /// The scheduler agent: healed at every launch, Settings' scheduler control.
    private var agent: SchedulerAgent?
    /// The launch's heal; a `--scheduled` instance quits only after it.
    private var healing: Task<Void, Never>?

    func applicationWillFinishLaunching(_ notification: Notification) {
        // Before launch ends, so a notification response that launched the app arrives.
        UNUserNotificationCenter.current().delegate = self
        SystemNotifications.registerCategory()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        AppLog.write("launch \(version ?? "?") \(Bundle.main.bundlePath) arguments: \(CommandLine.arguments.dropFirst())")
        var runner: CLIRunner?
        do {
            let cli = try HelperLocator.cli(bundleURL: Bundle.main.bundleURL, mainExecutable: Bundle.main.executableURL)
            runner = CLIRunner(executable: cli)
        } catch {
            AppLog.write("the embedded CLI is unavailable: \(error)")
        }
        let data = DataDirectory.current()
        let preferences = AppPreferences()
        // The window opens on what the popover shows, and its clean turns the
        // popover's icon; the popover is its router.
        let review = ReviewModel(
            runner: runner,
            data: data,
            preferences: preferences,
            snapshot: { [weak self] in self?.model?.snapshot },
            present: { [weak self] in
                guard let self, let review = self.review else { return false }
                return self.reviewWindow.show(review)
            },
            setBusy: { [weak self] in self?.model?.setCleaning($0) },
            rescan: { [weak self] in await self?.model?.scanAfterClean() },
            reloadReport: { [weak self] in await self?.model?.reloadReport() ?? false },
            paused: { [weak self] in self?.model?.paused ?? false },
            log: { AppLog.write($0) }
        )
        self.review = review
        // In every launch mode, so a promoted scheduled instance shows its running scan.
        model = PopoverModel(
            runner: runner,
            data: data,
            probe: FullDiskAccessProbe(path: Self.tccDatabase),
            preferences: preferences,
            router: review,
            nextScan: { [weak self] in self?.work?.next() ?? .off },
            legacyAgentFound: { [weak self] in self?.agent?.legacyAgentFound ?? false },
            // The build script keeps the app's version equal to its CLI's.
            appVersion: version,
            log: { AppLog.write($0) }
        )
        if let model {
            let disk = DiskModel(runner: runner, data: data, log: { AppLog.write($0) })
            self.disk = disk
            // `brew cleanup` measured the Homebrew cache finding again: the popover shows the edited report.
            disk.scanReportEdited = { _ = await model.reloadReport() }
            let agents = AgentsModel(runner: runner, log: { AppLog.write($0) })
            diskWindow = DiskWindowController(presence: presence, model: disk, agents: agents, popover: model)
            reviewWindow.wholeDisk = { DiskView(model: disk, agents: agents, popover: model) }
            // The store's writes reach the popover at once: ignored findings and the icon follow, with no scan.
            let store = SettingsStore(
                url: data.settings,
                changed: { [weak model] in Task { await model?.refresh() } },
                log: { AppLog.write($0) }
            )
            let agent = SchedulerAgent(
                service: SystemAgentService(),
                store: store,
                preferences: preferences,
                runner: runner,
                legacyPlist: LegacyAgent.plist(home: FileManager.default.homeDirectoryForCurrentUser),
                bundlePath: Bundle.main.bundlePath,
                notice: { [weak model] in model?.setNotice(id: PopoverModel.schedulerNotice, text: $0) },
                // The migration flow also asks for notification permission.
                askPermission: { [weak self] in await self?.work?.allowNotifications() },
                log: { AppLog.write($0) }
            )
            let work = ScheduledWork(
                popover: model,
                preferences: preferences,
                runner: runner,
                notifications: SystemNotifications(),
                legacyAgentFound: { [weak agent] in agent?.legacyAgentFound ?? false },
                openNotificationSettings: Self.openNotificationSettings,
                log: { AppLog.write($0) }
            )
            self.agent = agent
            self.work = work
            model.onNoticeAction = { [weak model] action in
                switch action {
                case .allowNotifications:
                    Task { await work.allowNotifications() }
                case .replaceOldScan:
                    Task {
                        await agent.replaceLegacy()
                        await model?.refresh()
                    }
                }
            }
            let settingsModel = SettingsModel(
                store: store,
                popover: model,
                preferences: preferences,
                scheduler: agent,
                commandLineTool: HelperLocator.commandLineTool(bundleURL: Bundle.main.bundleURL)
            )
            settings = SettingsWindowController(presence: presence, model: settingsModel, privacy: Self.privacy)
            // At every launch with scheduled scans on: register (idempotent), which
            // recovers an agent approved later or dropped when brew replaced the bundle.
            healing = Task { await agent.heal() }
        }
        NSApp.mainMenu = MainMenu.make(
            about: { [weak self] in self?.presence.showAbout() },
            settings: { [weak self] in self?.settings?.show() },
            find: { [weak self] in self?.reviewWindow.find() }
        )
        apply(launchMode.handle(.launched(arguments: CommandLine.arguments)))
    }

    /// Quit during a clean stops it after the current item, then quits: the
    /// CLI is a child process and would go on cleaning unwatched.
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard let review, review.prepareToQuit({ NSApp.reply(toApplicationShouldTerminate: true) }) else { return .terminateNow }
        AppLog.write("quit during a clean: stopping it after the current item")
        return .terminateLater
    }

    func applicationWillTerminate(_ notification: Notification) {
        // The last lines of a scheduled instance are the spike's evidence.
        AppLog.flush()
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        AppLog.write("reopen in \(launchMode.mode) mode")
        apply(launchMode.handle(.reopen(windowOpen: presence.isRegular)))
        return false
    }

    func applicationDidBecomeActive(_ notification: Notification) {
        AppLog.write("activate in \(launchMode.mode) mode")
        apply(launchMode.handle(.activate))
        if let model { Task { await model.refresh() } }
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse) async {
        let action = response.actionIdentifier
        await answer(action)
    }

    /// Review (or a click on the notification) and Remind Me Tomorrow. If the
    /// app was quit, macOS launched it in normal mode to deliver this.
    private func answer(_ action: String) async {
        AppLog.write("notification response \(action) in \(launchMode.mode) mode")
        apply(launchMode.handle(.notificationResponse))
        guard let work else { return }
        switch action {
        case SystemNotifications.remindAction:
            await work.remindTomorrow()
        case SystemNotifications.reviewAction, UNNotificationDefaultActionIdentifier:
            await work.review()
        default:
            break
        }
    }

    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .list]
    }

    // MARK: - Effects

    private func apply(_ effects: [LaunchMode.Effect]) {
        guard let model else { return }
        for effect in effects {
            switch effect {
            case .showStatusItem:
                guard statusItem == nil else { continue }
                // `open -j` launches the scheduled instance hidden.
                if NSApp.isHidden { NSApp.unhideWithoutActivation() }
                let statusItem = StatusItemController(
                    model: model,
                    openSettings: { [weak self] in self?.settings?.show() },
                    openNextScan: { [weak self] destination in self?.settings?.show(pane: .init(destination)) },
                    openPrivacy: Self.privacy.openFullDiskAccess,
                    openWholeDisk: { [weak self] in self?.diskWindow?.show() }
                )
                self.statusItem = statusItem
                reviewWindow.returnToPopover = { [weak statusItem] in statusItem?.showPopover() }
                AppLog.write("status item shown")
                Task {
                    await model.start()
                    // The first normal launch after the first scan asks once; a tick never does.
                    await work?.askPermissionOnce()
                }
            case .startScheduledWork:
                // A scan if due, then the pending notification and its ack; quits after all.
                Task {
                    await healing?.value
                    await work?.tick()
                    apply(launchMode.handle(.workFinished))
                }
            case .scanIfDue:
                AppLog.write("reopen of the running app: scan if due")
                Task { await work?.tick() }
            case .frontWindows:
                // A Dock click activates the app; the hourly tick's `open -g` does not.
                if NSApp.isActive { presence.frontAll() }
            case .quit:
                Task { await quitAfterScheduledWork() }
            }
        }
    }

    private func quitAfterScheduledWork() async {
        #if DEBUG
        // The spike's promotion check needs a scheduled instance that outlives
        // a scan of a near-empty home: `defaults write com.smartandpoint.deepdrivecleaner DebugHoldSeconds 120`.
        let hold = UserDefaults.standard.double(forKey: "DebugHoldSeconds")
        if hold > 0 {
            AppLog.write("DebugHoldSeconds: holding \(hold) s before quitting")
            try? await Task.sleep(for: .seconds(hold))
        }
        #endif
        // A user's event handled since the work ended has promoted the instance.
        guard launchMode.mode == .scheduled else {
            AppLog.write("promoted before quitting: keeps running")
            return
        }
        AppLog.write("scheduled work done: quitting")
        NSApp.terminate(nil)
    }

    // MARK: - System Settings

    static let privacy = PrivacySettings(
        openFullDiskAccess: { openPrivacySettings("Privacy_AllFiles") },
        openFilesAndFolders: { openPrivacySettings("Privacy_FilesAndFolders") }
    )

    /// System Settings → Notifications: once the permission is denied,
    /// macOS asks no more, so Allow Notifications opens it.
    static func openNotificationSettings() {
        let candidate = "x-apple.systempreferences:com.apple.Notifications-Settings.extension"
        if let url = URL(string: candidate), NSWorkspace.shared.open(url) {
            AppLog.write("opened \(candidate)")
        } else {
            AppLog.write("could not open the Notifications settings")
        }
    }

    /// Privacy & Security → `anchor` (Full Disk Access, Files and Folders).
    /// The anchor's form changed with System Settings; the manual check on
    /// macOS 26 tells which one it honours.
    static func openPrivacySettings(_ anchor: String) {
        let candidates = [
            "x-apple.systempreferences:com.apple.preference.security?\(anchor)",
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?\(anchor)",
            "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension",
        ]
        for candidate in candidates {
            if let url = URL(string: candidate), NSWorkspace.shared.open(url) {
                AppLog.write("opened \(candidate)")
                return
            }
        }
        AppLog.write("could not open the \(anchor) settings")
    }
}
