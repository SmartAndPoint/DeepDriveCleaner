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
        guard canFree, let runner else { return }
        freeSheet = FreeSheet(offer: offer, stage: .previewing)
        let ran = await runFree(runner, offer, dryRun: true)
        // Closed meanwhile: its answer goes nowhere.
        guard freeSheet == FreeSheet(offer: offer, stage: .previewing) else { return }
        switch ran {
        case .report(_, let action): freeSheet?.stage = .confirm(action)
        case .failed(let why): freeSheet?.stage = .failed(why)
        }
    }

    /// The sheet's confirm button: runs the action its dry run showed, and
    /// nothing else. Afterwards the view reads `last-disk.json` again, and
    /// the popover `last-scan.json` when `brew cleanup` edited it.
    public func freeConfirmedActions() async {
        guard let sheet = freeSheet, case .confirm(let preview) = sheet.stage, preview.result == .wouldRun, !scanning,
              let runner
        else {
            return
        }
        freeSheet?.stage = .running
        log("area action started: \(sheet.offer.id)")
        let ran = await runFree(runner, sheet.offer, dryRun: false)
        switch ran {
        case .report(let report, let action):
            freeSheet?.stage = .result(action)
            freeResults[sheet.offer.id] = action
            log("area action ended: \(sheet.offer.id) \(action.result)")
            if report.lastScan == .edited { await scanReportEdited?() }
        case .failed(let why):
            freeSheet?.stage = .failed(why)
            log("area action failed: \(sheet.offer.id): \(why)")
        }
        await load()
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

    private enum Ran {
        case report(FreeReport, FreeReport.ActionReport)
        case failed(String)
    }

    private func runFree(_ runner: any CLIRunning, _ offer: AreaActions.Offer, dryRun: Bool) async -> Ran {
        guard let file = writeActions([offer.listed]) else { return .failed("The confirmed action could not be written.") }
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
                guard let action = report.actions.first, report.actions.count == 1, report.isDryRun == dryRun else {
                    return .failed("The CLI's report does not match the confirmed action.")
                }
                return .report(report, action)
            } catch {
                log("disk free report unreadable: \(error)")
                return .failed("The CLI's report could not be read.")
            }
        }
        if result.outcome != .done { log("disk free ended \(result.outcome): \(result.stderrTail.suffix(3).joined(separator: " | "))") }
        return .failed(Self.freeFailure(result.outcome, stderrTail: result.stderrTail))
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

    /// One sentence; `name` is the offer's ("iOS 27.0 (24A100)", "Homebrew").
    public static func result(_ action: FreeReport.ActionReport, name: String) -> String {
        switch action.result {
        case .freed:
            return action.freedBytes.map { "Freed \(ByteSize.human($0))." } ?? "Done."
        case .removedNotFreed:
            let size = action.bytesAfter.map { " (\(ByteSize.human($0)))" } ?? ""
            return "The runtime is gone from simctl's list, but its folder did not get smaller\(size). macOS may free it later."
        case .stillDeleting:
            return "CoreSimulator is still deleting it. Scan the whole disk again later to see the space."
        case .refused(let reason):
            return "Not run: \(reason.text)."
        case .failed(let needsAdmin):
            let tool = action.action == .brewCleanup ? "brew cleanup" : "simctl"
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
}
