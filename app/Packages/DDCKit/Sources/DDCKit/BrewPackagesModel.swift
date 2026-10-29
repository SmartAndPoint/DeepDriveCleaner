import Foundation
import Observation

/// The Homebrew packages sheet's state (story "Whole disk: Homebrew
/// packages, their sizes, and brew uninstall"): `ddc disk brew-packages
/// --json` in the runner's `brewPackages` lane, the Uninstall… offer of each
/// formula brew lets go, and the orphans step after one was uninstalled.
///
/// It lists and removes nothing itself: an uninstall goes through
/// `DiskModel.previewAction` and the area action sheet, whose CLI checks
/// everything again right before brew runs. Only the Homebrew row's button
/// opens it; the scheduled path never does.
@MainActor
@Observable
public final class BrewPackagesModel {
    public static let arguments = ["disk", "brew-packages", "--json"]

    /// The sheet shows.
    public private(set) var isOpen = false
    /// The last listing; nil before the first, or when it failed.
    public private(set) var listing: BrewPackages?
    /// brew is being asked.
    public private(set) var asking = false
    /// Why the last listing gave nothing.
    public private(set) var message: String?
    /// After a formula was uninstalled: what it alone needed and nothing
    /// needs now, offered once.
    public private(set) var orphans: AreaActions.Offer?

    @ObservationIgnored private let runner: (any CLIRunning)?
    @ObservationIgnored private let log: @MainActor (String) -> Void
    @ObservationIgnored private var listTask: Task<Void, Never>?

    init(runner: (any CLIRunning)?, log: @escaping @MainActor (String) -> Void) {
        self.runner = runner
        self.log = log
    }

    /// The Homebrew row's button: opens the sheet and asks brew.
    public func open() async {
        isOpen = true
        orphans = nil
        await list()
    }

    /// Closes the sheet; a listing still going ends on its own and is kept.
    public func close() {
        isOpen = false
        orphans = nil
    }

    /// One listing at a time: a second request joins the running one.
    public func list() async {
        if let listTask {
            await listTask.value
            return
        }
        guard let runner else {
            message = "The app's embedded CLI is missing, so it cannot ask Homebrew. Reinstall DeepDriveCleaner."
            return
        }
        let task = Task { await run(runner) }
        listTask = task
        await task.value
        listTask = nil
    }

    /// The Uninstall… offer of `package`, with its kegs as listed; nil when
    /// the listing says why not, or brew listed a formula ddc cannot read.
    public func offer(_ package: BrewPackages.Package) -> AreaActions.Offer? {
        guard let listing, listing.incomplete.isEmpty, package.notOffered == nil, !package.kegs.isEmpty else { return nil }
        return AreaActions.Offer(
            action: .brewUninstall, identifier: package.fullName, name: package.fullName, bytes: package.bytes,
            kegs: package.kegs.map(\.version)
        )
    }

    /// After the sheet's dry run or run of a Homebrew offer ends: lists
    /// again unless the dry run found nothing changed. After a `freed`
    /// uninstall of a formula, offers what it alone needed that the fresh
    /// listing finds needed by nothing. After an orphans run that did not
    /// free them all, offers again those the fresh listing still finds
    /// needed by nothing; brew stops at the first it cannot remove.
    func actionEnded(_ offer: AreaActions.Offer, _ action: FreeReport.ActionReport?, dryRun: Bool) async {
        guard isOpen, offer.action == .brewUninstall || offer.action == .brewUninstallOrphans else { return }
        if dryRun, action?.result == .wouldRun { return }
        let before = listing?.formulae.first { $0.fullName == offer.identifier }
        if !dryRun || offer.action == .brewUninstallOrphans { orphans = nil }
        await list()
        guard let fresh = listing, fresh.incomplete.isEmpty, fresh.services != .unreadable else { return }
        if offer.action == .brewUninstallOrphans {
            guard action?.result != .freed else { return }
            let names = (offer.names ?? []).filter(fresh.orphans.contains)
            guard !names.isEmpty else { return }
            let sizes = names.map { name in fresh.formulae.first { $0.fullName == name }?.bytes }
            let bytes = names == offer.names ? offer.bytes : sizes.contains(nil) ? nil : sizes.reduce(UInt64(0)) { $0 + ($1 ?? 0) }
            orphans = AreaActions.Offer(action: .brewUninstallOrphans, identifier: nil, name: offer.name, bytes: bytes, names: names)
            log("brew packages: \(names.count) still needed by nothing")
            return
        }
        guard !dryRun, action?.result == .freed, let before else { return }
        let names = before.wouldOrphan.filter(fresh.orphans.contains)
        guard !names.isEmpty else { return }
        let sizes = names.map { name in before.dependencies.first { $0.fullName == name }?.bytes }
        let bytes = sizes.contains(nil) ? nil : sizes.reduce(UInt64(0)) { $0 + ($1 ?? 0) }
        orphans = AreaActions.Offer(
            action: .brewUninstallOrphans, identifier: nil, name: "what \(before.fullName) needed", bytes: bytes, names: names
        )
        log("brew packages: \(names.count) needed by nothing after \(before.fullName)")
    }

    private func run(_ runner: any CLIRunning) async {
        message = nil
        asking = true
        defer { asking = false }
        let result = await runner.run(.brewPackages, Self.arguments, options: CLIRunner.Options())
        guard result.outcome == .done else {
            // An old list would offer what may be gone.
            listing = nil
            message = Self.failure(result.outcome, stderrTail: result.stderrTail)
            log("disk brew-packages ended \(result.outcome): \(result.stderrTail.suffix(3).joined(separator: " | "))")
            return
        }
        let stdout = result.stdout
        switch await Task.detached(operation: { Result { try BrewPackages.read(stdout) } }).value {
        case .success(let read):
            listing = read
        case .failure(ScanReport.ReadError.otherSchema(let version)):
            listing = nil
            message = "The CLI wrote another version of the list (schema \(version))."
        case .failure(let error):
            listing = nil
            message = "The CLI's list could not be read."
            log("disk brew-packages unreadable: \(error)")
        }
    }

    static func failure(_ outcome: RunOutcome, stderrTail: [String]) -> String {
        switch outcome {
        case .done:
            return "The CLI's list could not be read."
        case .cancelled:
            return "The listing was stopped."
        case .error(let status):
            let error = stderrTail.last { $0.hasPrefix("ddc: ") && !$0.hasPrefix("ddc: warning:") }
            return error.map { String($0.dropFirst("ddc: ".count)) } ?? "Listing the Homebrew packages failed (exit \(status))."
        case .busy:
            return "Listing the Homebrew packages reported busy (exit 75), which it should never do."
        case .refused:
            return "The CLI refused to list the Homebrew packages (exit 2): a bug in the app."
        case .crashed(let signal):
            return "Listing the Homebrew packages stopped unexpectedly (signal \(signal))."
        case .notLaunched(let reason):
            return "Listing the Homebrew packages could not start: \(reason)."
        }
    }
}
