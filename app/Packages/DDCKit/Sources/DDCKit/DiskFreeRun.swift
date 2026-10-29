import Foundation

/// The only code that runs `disk free`, its dry run included (story "Whole
/// disk: free space with the owning tool"; menu bar ADR §6 item 11, §7). The
/// sheet's confirm button is the one caller of `freeConfirmedActions()`; a
/// structural test holds both. Both runs go through the runner's queue: the
/// run takes the exclusive lock, so a scan asked for meanwhile waits for it.
extension DiskModel {
    /// Opens the sheet of `offer` and runs its tool's own dry run, which the
    /// sheet shows before anything can be confirmed.
    public func previewAction(_ offer: AreaActions.Offer) async {
        await preview([offer])
    }

    /// Opens the sheet of `offers`, in run order, and runs their tools' own
    /// dry runs in one call, which the sheet shows before anything can be
    /// confirmed.
    private func preview(_ offers: [AreaActions.Offer]) async {
        guard canFree, !offers.isEmpty, let runner else { return }
        freeSheet = FreeSheet(offers: offers, stage: .previewing)
        let ran = await runFree(runner, offers, dryRun: true)
        // Closed meanwhile: its answer goes nowhere.
        guard freeSheet == FreeSheet(offers: offers, stage: .previewing) else { return }
        switch ran {
        case .report(let report):
            freeSheet?.stage = .confirm(report.actions)
            for (offer, action) in zip(offers, report.actions) {
                await brewPackages.actionEnded(offer, action, dryRun: true)
            }
        case .failed(let why):
            freeSheet?.stage = .failed(why)
            for offer in offers {
                await brewPackages.actionEnded(offer, nil, dryRun: true)
            }
        }
    }

    /// The sheet's confirm button: runs the actions its dry run showed, and
    /// nothing else. Afterwards the view reads `last-disk.json` again, and
    /// the popover `last-scan.json` when `brew cleanup` edited it.
    public func freeConfirmedActions() async {
        guard let sheet = freeSheet, case .confirm(let previews) = sheet.stage,
              previews.allSatisfy({ $0.result == .wouldRun }), !scanning, let runner
        else {
            return
        }
        let offers = sheet.offers
        freeSheet?.stage = .running
        log("area action started: \(Self.ids(offers))")
        let ran = await runFree(runner, offers, dryRun: false)
        var ended: [FreeReport.ActionReport?] = Array(repeating: nil, count: offers.count)
        switch ran {
        case .report(let report):
            freeSheet?.stage = .result(report.actions)
            for (index, (offer, action)) in zip(offers, report.actions).enumerated() {
                freeResults[offer.id] = action
                ended[index] = action
                log("area action ended: \(offer.id) \(action.result)")
            }
            if report.lastScan == .edited { await scanReportEdited?() }
        case .failed(let why):
            freeSheet?.stage = .failed(why)
            log("area action failed: \(Self.ids(offers)): \(why)")
        }
        await load()
        // The packages sheet lists again behind the result.
        for (offer, action) in zip(offers, ended) {
            await brewPackages.actionEnded(offer, action, dryRun: false)
        }
    }

    private static func ids(_ offers: [AreaActions.Offer]) -> String {
        offers.map(\.id).joined(separator: ", ")
    }

    /// Stop in the sheet: SIGTERM to the run, or it never starts if it still
    /// waits. The CLI ends a deletion it already started as "still deleting".
    public func stopFree() async {
        guard let freeTicket, let runner else { return }
        log("area action: stop")
        await runner.cancel(freeTicket)
    }

    /// Closes the sheet; a dry run still going is stopped. A run is not:
    /// the sheet stays until it ends.
    public func closeFree() async {
        guard !freeRunning else { return }
        if freeSheet?.stage == .previewing, let freeTicket, let runner { await runner.cancel(freeTicket) }
        freeSheet = nil
    }

    /// The sheet's own dismissal (Escape): closes it at once, as the
    /// presentation expects, and stops a dry run still going after. A run
    /// is not: nothing changes until it ends.
    public func dismissFree() {
        guard !freeRunning else { return }
        let ticket = freeSheet?.stage == .previewing ? freeTicket : nil
        freeSheet = nil
        if let ticket, let runner { Task { await runner.cancel(ticket) } }
    }

    private enum Ran {
        /// One action report per offer, in the list's order.
        case report(FreeReport)
        case failed(String)
    }

    /// One call with the whole list. A report whose actions do not match the
    /// list one for one, in count, order, action and identifier, is a
    /// failure, never a partial result.
    private func runFree(_ runner: any CLIRunning, _ offers: [AreaActions.Offer], dryRun: Bool) async -> Ran {
        guard let file = writeActions(offers.map(\.listed)) else {
            return .failed("The confirmed \(offers.count == 1 ? "action" : "actions") could not be written.")
        }
        defer { try? FileManager.default.removeItem(at: file) }
        let ticket = CLIRunner.Ticket()
        freeTicket = ticket
        defer { if freeTicket == ticket { freeTicket = nil } }
        let arguments = ["disk", "free", "--actions", file.path] + (dryRun ? ["--dry-run"] : []) + ["--json"]
        let result = await runner.run(dryRun ? .diskFreeDryRun : .diskFree, arguments, options: CLIRunner.Options(ticket: ticket))
        // A stopped run prints its report before it ends by the signal.
        if result.outcome == .done || result.outcome == .cancelled, !result.stdout.isEmpty {
            do {
                let report = try FreeReport.read(result.stdout)
                guard report.isDryRun == dryRun, Self.matches(report.actions, offers) else {
                    return .failed("The CLI's report does not match the confirmed \(offers.count == 1 ? "action" : "actions").")
                }
                return .report(report)
            } catch {
                log("disk free report unreadable: \(error)")
                return .failed("The CLI's report could not be read.")
            }
        }
        if result.outcome != .done { log("disk free ended \(result.outcome): \(result.stderrTail.suffix(3).joined(separator: " | "))") }
        return .failed(Self.freeFailure(result.outcome, stderrTail: result.stderrTail))
    }

    /// One report per offer, the same action on the same identifier, in the
    /// same order.
    static func matches(_ actions: [FreeReport.ActionReport], _ offers: [AreaActions.Offer]) -> Bool {
        actions.count == offers.count
            && zip(actions, offers).allSatisfy { $0.action == $1.action && $0.identifier == $1.identifier }
    }

    /// The list goes to a file: the runner's stdin is `/dev/null`. Unique per
    /// run, readable by the user only, removed when the run ends.
    private func writeActions(_ actions: [ListedAreaAction]) -> URL? {
        let file = temporaryDirectory.appending(path: "ddc-actions-\(UUID().uuidString).json")
        do {
            let data = try AreaActionList(actions: actions).encoded()
            guard FileManager.default.createFile(atPath: file.path, contents: data, attributes: [.posixPermissions: 0o600]) else {
                return nil
            }
            return file
        } catch {
            log("action list not written: \(error)")
            return nil
        }
    }

    static func freeFailure(_ outcome: RunOutcome, stderrTail: [String]) -> String {
        switch outcome {
        case .done:
            return "The CLI printed no report."
        case .cancelled:
            return "Stopped before anything ran."
        case .busy:
            return "Another scan or clean holds the lock, perhaps from Terminal. Try again when it ends."
        case .refused:
            let why = stderrTail.last { $0.hasPrefix("ddc disk free: ") }.map { ": " + $0.dropFirst("ddc disk free: ".count) } ?? ""
            return "The CLI refused the action (exit 2)\(why)."
        case .error(let status):
            let error = stderrTail.last { $0.hasPrefix("ddc: ") && !$0.hasPrefix("ddc: warning:") }
            return error.map { String($0.dropFirst("ddc: ".count)) } ?? "The action failed (exit \(status))."
        case .crashed(let signal):
            return "The CLI stopped unexpectedly (signal \(signal))."
        case .notLaunched(let reason):
            return "The CLI could not start: \(reason)."
        }
    }
}

/// The sheet's and the row's sentences about a result.
public enum FreeWording {
    /// `free::STOPPED`: the CLI's output of a tool it stopped.
    static let stopped = "stopped before the tool finished"
    /// `free::NOT_STARTED`: brew's re-read before an uninstall could not be
    /// made, or left too little time; the next line says why.
    static let notStarted = "brew could not start; nothing was removed"

    /// One sentence; `name` is the offer's ("iOS 27.0 (24A100)", "Homebrew", "ml").
    public static func result(_ action: FreeReport.ActionReport, name: String) -> String {
        switch action.result {
        case .freed:
            return action.freedBytes.map { "Freed \(ByteSize.human($0))." } ?? "Done."
        case .removedNotFreed where action.action == .condaEnvRemove:
            return "conda finished, but the environment's folder is still there: nothing was freed. Its folder may need an administrator; you can run the command in Terminal to see why."
        case .removedNotFreed where action.area == "homebrew":
            return "brew finished, but a version is still in /opt/homebrew/Cellar: not all of it was freed. You can run the command in Terminal to see why."
        case .removedNotFreed:
            let size = action.bytesAfter.map { " (\(ByteSize.human($0)))" } ?? ""
            return "The runtime is gone from simctl's list, but its folder did not get smaller\(size). macOS may free it later."
        case .stillDeleting:
            return "CoreSimulator is still deleting it. Scan the whole disk again later to see the space."
        case .refused(let reason):
            return "Not run: \(reason.text)."
        case .failed(let needsAdmin):
            let tool = toolName(action.action)
            if let output = action.output, output.hasPrefix(notStarted) {
                let why = output.split(separator: "\n", maxSplits: 1).dropFirst().first.map { " \($0)." } ?? ""
                return "brew could not start, so nothing was removed.\(why)"
            }
            if action.output?.hasPrefix(stopped) == true {
                return "Stopped before \(tool) finished. Part of it may have been removed; scan the whole disk again to see the space."
            }
            return needsAdmin
                ? "\(tool) could not remove everything without an administrator. You can run the command in Terminal."
                : "\(tool) failed. You can run the command in Terminal to see why."
        case .wouldRun:
            return "Nothing has run yet."
        case .notRun:
            return "Stopped before it started: nothing ran."
        }
    }

    /// Whether the sheet shows the tool's output and the command to run in
    /// Terminal: after a failure, unless brew's check before an uninstall
    /// kept it from starting, and after conda left an environment or brew
    /// a keg in place, which they do without an error.
    public static func offersTerminal(_ action: FreeReport.ActionReport) -> Bool {
        switch action.result {
        case .failed: action.output?.hasPrefix(notStarted) != true
        case .removedNotFreed: action.action == .condaEnvRemove || action.area == "homebrew"
        default: false
        }
    }

    /// Whether the sheet shows the tool's output's tail: with the Terminal
    /// command, and after any brew uninstall that ran.
    public static func showsOutput(_ action: FreeReport.ActionReport) -> Bool {
        if offersTerminal(action) { return true }
        guard action.action == .brewUninstall || action.action == .brewUninstallOrphans else { return false }
        return action.result == .freed || action.result == .removedNotFreed
    }

    /// The tool as the user would type it.
    public static func toolName(_ action: DiskReport.ActionId) -> String {
        switch action {
        case .simctlRuntimeDelete: "simctl"
        case .brewCleanup: "brew cleanup"
        case .condaEnvRemove: "conda env remove"
        case .condaClean: "conda clean"
        case .brewUninstall, .brewUninstallOrphans: "brew uninstall"
        }
    }
}
