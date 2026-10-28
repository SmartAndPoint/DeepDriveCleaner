import Foundation

/// An agent's state as `SMAppService.Status` gives it.
public enum AgentStatus: Sendable, Equatable {
    case notRegistered, enabled, requiresApproval, notFound
}

/// `SMAppService.agent(plistName:)` (in the app target; DDCKit never imports
/// ServiceManagement, and tests use a fake).
@MainActor
public protocol AgentService: AnyObject {
    func status(_ plist: String) -> AgentStatus
    func register(_ plist: String) throws
    func unregister(_ plist: String) async throws
}

/// The scheduler agent of menu bar ADR §5: an hourly tick registered from
/// the app's bundle, which heals itself at every launch, records itself in
/// `settings.json` so `ddc schedule install` refuses, and replaces the 0.1.0
/// agent when the user agrees. It is Settings' `SchedulerControl`.
@MainActor
public final class SchedulerAgent: SchedulerControl {
    /// The bundle program `DeepDriveCleaner --tick` (B6).
    public static let primaryPlist = "com.smartandpoint.deepdrivecleaner.scheduler.plist"
    /// `/usr/bin/open -gjb <id> --args --scheduled`, when the primary is refused (B6).
    public static let fallbackPlist = "com.smartandpoint.deepdrivecleaner.scheduler-open.plist"
    /// Both plists refused.
    public static let refused = "macOS did not allow the scheduled scan. Allow DeepDriveCleaner in System Settings → General → Login Items & Extensions, or run ddc schedule install in Terminal."

    private let service: any AgentService
    private let store: SettingsStore
    private let preferences: AppPreferences
    private let runner: (any CLIRunning)?
    private let legacyPlist: URL
    private let bundlePath: String
    private let notice: @MainActor (String?) -> Void
    private let askPermission: @MainActor () async -> Void
    private let log: @MainActor (String) -> Void
    /// The last change Settings asked for; each waits for the one before,
    /// so an off then on never races its own unregister. A test waits for it.
    public private(set) var change: Task<Void, Never>?
    /// Why the last Replace It changed nothing; Settings → Permissions shows it.
    public private(set) var replaceFailure: String?

    public init(
        service: any AgentService,
        store: SettingsStore,
        preferences: AppPreferences,
        runner: (any CLIRunning)?,
        legacyPlist: URL,
        bundlePath: String,
        notice: @escaping @MainActor (String?) -> Void = { _ in },
        askPermission: @escaping @MainActor () async -> Void = {},
        log: @escaping @MainActor (String) -> Void = { _ in }
    ) {
        self.service = service
        self.store = store
        self.preferences = preferences
        self.runner = runner
        self.legacyPlist = legacyPlist
        self.bundlePath = bundlePath
        self.notice = notice
        self.askPermission = askPermission
        self.log = log
    }

    public var legacyAgentFound: Bool { FileManager.default.fileExists(atPath: legacyPlist.path) }

    /// The plist in use: the primary, unless only the fallback is registered.
    private var active: String {
        let primary = service.status(Self.primaryPlist)
        let fallback = service.status(Self.fallbackPlist)
        return primary == .notRegistered && fallback != .notRegistered ? Self.fallbackPlist : Self.primaryPlist
    }

    public var registration: SchedulerRegistration? {
        switch service.status(active) {
        case .enabled: .allowed
        case .requiresApproval: .requiresApproval
        case .notRegistered, .notFound: .off
        }
    }

    // MARK: - Healing

    /// At every launch, and when Settings turns scheduled scans on or off:
    /// on registers (idempotent), off unregisters. Nothing while the 0.1.0
    /// agent exists (B1). The record follows the status after the call,
    /// not whether the call threw.
    public func heal() async {
        guard !legacyAgentFound else {
            log("scheduler: the 0.1.0 agent exists; the app's agent is not registered")
            return
        }
        guard preferences.schedule.enabled else {
            for plist in [Self.primaryPlist, Self.fallbackPlist] where service.status(plist) != .notRegistered {
                do { try await service.unregister(plist) } catch { log("scheduler: unregister \(plist) failed: \(error)") }
            }
            setRecord(false)
            notice(nil)
            log("scheduler: scheduled scans off; unregistered")
            return
        }
        var plist = Self.primaryPlist
        var status = register(plist)
        if status != .enabled, status != .requiresApproval {
            log("scheduler: the bundle program was refused (\(status)); trying open")
            try? await service.unregister(plist)
            plist = Self.fallbackPlist
            status = register(plist)
        } else if service.status(Self.fallbackPlist) != .notRegistered {
            // Two ticks an hour would scan twice.
            try? await service.unregister(Self.fallbackPlist)
        }
        switch status {
        case .enabled:
            setRecord(true)
            notice(nil)
        case .requiresApproval:
            notice(nil)
        case .notRegistered, .notFound:
            setRecord(false)
            notice(Self.refused)
        }
        log("scheduler: \(plist) \(status)")
    }

    private func register(_ plist: String) -> AgentStatus {
        do {
            try service.register(plist)
        } catch {
            log("scheduler: register \(plist) threw \(error)")
        }
        return service.status(plist)
    }

    /// `scheduler: {by: app, app: <bundle>}`, written only when it changes.
    private func setRecord(_ on: Bool) {
        let record = on ? SettingsFile.Scheduler(by: "app", app: bundlePath) : nil
        // The store reads the file first and writes nothing when it holds this record already.
        store.update { $0.scheduler = record }
    }

    public func scheduleChanged(_ schedule: Schedule) {
        // The tick is hourly whatever the hour: only on and off reach the agent.
        let previous = change
        change = Task {
            await previous?.value
            await heal()
        }
    }

    // MARK: - Migration

    /// Replace It: the old agent's interval and threshold, read before the
    /// uninstall deletes its plist; then the uninstall, which must succeed
    /// before anything changes; then the schedule, the threshold, the app's
    /// agent and the notification question.
    @discardableResult
    public func replaceLegacy() async -> Bool {
        guard let legacy = LegacyAgent.read(legacyPlist) else { return false }
        guard let runner else { return false }
        let result = await runner.run(.schedule, ["schedule", "uninstall"])
        guard result.outcome == .done else {
            log("migration: schedule uninstall \(result.outcome); nothing changed")
            let failure = ["The old background scan could not be removed.", CLIFailure.message(result.outcome, stderrTail: result.stderrTail)]
                .compactMap { $0 }.joined(separator: " ")
            replaceFailure = failure
            notice(failure)
            return false
        }
        replaceFailure = nil
        preferences.setSchedule(legacy.schedule)
        if let threshold = legacy.threshold { store.update { $0.notifyThreshold = threshold } }
        await heal()
        await askPermission()
        log("migration: replaced the 0.1.0 agent (\(legacy.weekly ? "weekly" : "daily"), threshold \(legacy.threshold.map(String.init) ?? "none"))")
        return true
    }

    public func replaceLegacyAgent() async {
        await replaceLegacy()
    }
}
