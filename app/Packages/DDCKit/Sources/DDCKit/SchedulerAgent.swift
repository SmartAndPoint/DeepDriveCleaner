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
    /// `CFBundleVersion` of the running app.
    private let bundleVersion: String
    /// False in Debug builds: a build run from Xcode never moves the agent
    /// onto a bundle that may be deleted, and records nothing.
    private let reregisters: Bool
    /// Whether a bundle is at a path; tests give their own answer.
    private let bundleExists: @MainActor (String) -> Bool
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
        bundleVersion: String = "0",
        reregisters: Bool = true,
        bundleExists: @escaping @MainActor (String) -> Bool = { FileManager.default.fileExists(atPath: $0) },
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
        self.bundleVersion = bundleVersion
        self.reregisters = reregisters
        self.bundleExists = bundleExists
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
    /// on registers (idempotent), and first unregisters after an update in
    /// place (`registrationStep`); off unregisters. Nothing while the 0.1.0
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
        let record = preferences.registeredBundle
        let settingsApp = self.settingsApp
        let decision = reregisters
            ? Self.registrationStep(
                record: record, recordExists: record.map { bundleExists($0.path) } ?? false,
                settingsApp: settingsApp, bundlePath: bundlePath, build: bundleVersion
            )
            : .leave
        switch decision {
        case .reregister:
            // launchd keeps the registration of the build that made it, and
            // cannot start another build's program (EX_CONFIG) while
            // `SMAppService` still says enabled: unregister, then register.
            log("scheduler: \(bundlePath) is build \(bundleVersion) now; registering the agent again")
            for plist in [Self.primaryPlist, Self.fallbackPlist] where service.status(plist) != .notRegistered {
                do { try await service.unregister(plist) } catch { log("scheduler: unregister \(plist) failed: \(error)") }
            }
        case .record:
            break
        case .leave:
            log(reregisters
                ? "scheduler: launched from \(bundlePath), registered from \(record?.path ?? settingsApp ?? "?"); not registering again"
                : "scheduler: a Debug build; not registering again")
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
        // Whatever the status: a re-registration that ends in approval pending
        // or a throw is not tried again at every launch of this build.
        if decision != .leave { preferences.setRegisteredBundle(RegisteredBundle(path: bundlePath, build: bundleVersion)) }
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

    /// What a launch with scheduled scans on does before it registers.
    public enum RegistrationStep: Sendable, Equatable {
        /// An update in place: unregister, then register.
        case reregister
        /// Register as always, and record this bundle and build.
        case record
        /// Another copy of the app (a Debug build, a copy elsewhere):
        /// register as always, record nothing.
        case leave
    }

    /// The same path with another build re-registers; the same build only
    /// registers. With no record yet, or one whose bundle is gone (the app
    /// moved from a disk image or Downloads), `settings.json`'s
    /// `scheduler.app` (written while the agent is enabled) stands for the
    /// path: the same path re-registers, none yet (a first registration)
    /// records. Another path leaves the registration where it is.
    public static func registrationStep(record: RegisteredBundle?, recordExists: Bool = true, settingsApp: String?, bundlePath: String, build: String) -> RegistrationStep {
        if let record, recordExists || record.path == bundlePath {
            guard record.path == bundlePath else { return .leave }
            return record.build == build ? .record : .reregister
        }
        switch settingsApp {
        case bundlePath?: return .reregister
        case nil: return .record
        default: return .leave
        }
    }

    /// `settings.json`'s `scheduler.app`, read fresh: `setRecord` rewrites it.
    private var settingsApp: String? {
        store.reload()
        return store.file.scheduler?.app
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

    /// The launch's heal, in the same queue as Settings' changes, so a
    /// switch to off during a re-registration waits for it.
    public func healAtLaunch() -> Task<Void, Never> {
        let previous = change
        let task = Task {
            await previous?.value
            await heal()
        }
        change = task
        return task
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
