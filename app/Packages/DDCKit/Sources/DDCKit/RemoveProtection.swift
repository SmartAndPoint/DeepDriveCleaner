import Foundation

/// Remove Protection… (story "Protected by you: say what it is, and Remove
/// Protection"; safety ADR and menu bar ADR §6 item 12, §7, amended
/// 2026-10-30): takes a protection away, never the folder. What lay beneath
/// is listed by the next scan in its own group and cleaned there, if at all.
///
/// The only code that runs `keep remove`. The confirmation's confirm button
/// is the one caller of `removeProtection(_:)`; a structural test holds both.
public enum RemoveProtection {
    /// The one protection of a row that Remove Protection… takes away.
    public enum Target: Sendable, Equatable {
        /// A `.ddc-keep`: `keep remove` with the folder's identity.
        case marker(ScanReport.MarkerFolder)
        /// An exclusion or skip of `settings.json`: the same edit as
        /// Settings → Folders → Remove.
        case settings(ScanReport.Exclusion)
    }

    /// Where a removal is, for its row.
    public struct Progress: Sendable, Equatable {
        public enum Stage: Sendable, Equatable {
            case removing, scanning
        }

        /// The row's id in the rows the removal started from.
        public let id: Int
        public let stage: Stage

        /// The row's second line meanwhile.
        public var text: String {
            switch stage {
            case .removing: "Removing the protection…"
            case .scanning: "Protection removed. Scanning…"
            }
        }
    }

    /// What the confirmation says before anything is removed.
    public struct Confirmation: Sendable, Equatable {
        /// The row it was opened on.
        public let id: Int
        public let target: Target
        public let question: String
        public let message: String
        /// Each kept finding it unprotects, with the rule that would claim it.
        public let findings: [String]
        /// Set when git is known to track the marker.
        public let gitWarning: String?
        public let confirmTitle: String
    }

    /// The row's protection when it has exactly one and it is not a flag's.
    /// A row with several, or with a command-line exclusion or skip, gets no
    /// Remove Protection…: its second line says how to remove them.
    public static func target(_ protection: ScanReport.Protection?) -> Target? {
        guard let protection, protection.markers.count + protection.exclusions.count == 1 else { return nil }
        if let marker = protection.markers.first { return .marker(marker) }
        guard let exclusion = protection.exclusions.first, exclusion.origin == .settings else { return nil }
        return .settings(exclusion)
    }

    /// Whether `protection` holds `target`: the findings one removal unprotects.
    static func holds(_ protection: ScanReport.Protection?, _ target: Target) -> Bool {
        guard let protection else { return false }
        return switch target {
        case .marker(let marker): protection.markers.contains(marker)
        case .settings(let exclusion): protection.exclusions.contains(exclusion)
        }
    }

    /// `keep remove` with the identity the scan recorded: the CLI asks
    /// nothing and refuses a folder that changed.
    static func arguments(_ marker: ScanReport.MarkerFolder) -> [String] {
        ["keep", "remove", marker.folder, "--folder-dev", String(marker.dev), "--folder-ino", String(marker.ino), "--json"]
    }

    /// The git warning of the confirmation.
    public static let gitTracked = "git tracks this .ddc-keep: someone may have committed it, and removing it changes the checkout."

    static func confirmation(id: Int, target: Target, findings: [ScanReport.Finding], git: GitTracking.Answer, home: String) -> Confirmation {
        let beneath = "At the next scan what lies beneath appears in its own group, where it may be selected for cleanup by default."
        let question: String
        let message: String
        switch target {
        case .marker(let marker):
            question = "Remove the .ddc-keep in \(Wording.tilde(marker.folder, home: home))?"
            message = "The folder stays as it is and is no longer protected. \(beneath)"
        case .settings(let exclusion):
            let shown = Wording.tilde(exclusion.path, home: home)
            question = exclusion.kind == .skip ? "Stop skipping \(shown)?" : "Stop excluding \(shown)?"
            message = "This removes \(shown) from Settings → Folders; the folder stays as it is. \(beneath)"
        }
        return Confirmation(
            id: id,
            target: target,
            question: question,
            message: message,
            findings: findings.map { finding in
                let path = Wording.tilde(finding.path, home: home)
                return finding.protection?.rule.map { "\(path) · otherwise: \(CleanWording.ruleName($0))" } ?? path
            },
            gitWarning: git == .tracked ? gitTracked : nil,
            confirmTitle: "Remove Protection"
        )
    }

    enum Ran {
        case report(KeepReport)
        case failed(String)
    }

    /// One `keep remove`, queued like a clean: it takes the exclusive lock.
    /// A report of another folder is a failure, never a result.
    static func run(_ runner: any CLIRunning, _ marker: ScanReport.MarkerFolder, log: @MainActor (String) -> Void) async -> Ran {
        let result = await runner.run(.keepRemove, arguments(marker), options: CLIRunner.Options())
        guard result.outcome == .done else {
            await log("keep remove ended \(result.outcome): \(result.stderrTail.suffix(3).joined(separator: " | "))")
            return .failed(failure(result.outcome, stderrTail: result.stderrTail))
        }
        do {
            let report = try KeepReport.read(result.stdout)
            guard report.path == marker.folder else { return .failed("The CLI's report is not of that folder. Scan Again to see whether it is protected.") }
            return .report(report)
        } catch {
            await log("keep remove report unreadable: \(error)")
            return .failed("The CLI's report could not be read. Scan Again to see whether the folder is still protected.")
        }
    }

    /// What the row's notice says when the marker stays.
    public static func text(_ result: KeepReport.Outcome, folder: String, home: String) -> String? {
        let shown = Wording.tilde(folder, home: home)
        return switch result {
        case .removed, .absent: nil
        case .refused(.outsideScanRoots): "\(shown) is outside the folders DeepDriveCleaner scans, so its .ddc-keep was not removed."
        case .refused(.changedSinceScan): "\(shown) changed since the scan; nothing was removed. Scan Again first."
        case .refused(.noFolder): "\(shown) is gone; nothing was removed."
        case .refused(.notAFile): "The .ddc-keep in \(shown) is not a file (a symlink or a folder), so it was not removed. Remove it in Finder."
        case .failed(let error): "The .ddc-keep in \(shown) could not be removed (\(Wording.escaped(error)))."
        }
    }

    /// A run that ended without a report: nothing was removed.
    static func failure(_ outcome: RunOutcome, stderrTail: [String]) -> String {
        switch outcome {
        case .done:
            return "The CLI printed no report."
        case .cancelled:
            return "Stopped before anything was removed."
        case .busy:
            return "Another scan or clean holds the lock, perhaps from Terminal. Try again when it ends."
        case .refused:
            let why = stderrTail.last { $0.hasPrefix("ddc keep remove: ") }.map { ": " + $0.dropFirst("ddc keep remove: ".count) } ?? ""
            return "The CLI refused it (exit 2)\(why)."
        case .error(let status):
            let error = stderrTail.last { $0.hasPrefix("ddc: ") && !$0.hasPrefix("ddc: warning:") }
            return error.map { String($0.dropFirst("ddc: ".count)) } ?? "Removing the protection failed (exit \(status))."
        case .crashed(let signal):
            return "The CLI stopped unexpectedly (signal \(signal))."
        case .notLaunched(let reason):
            return "The CLI could not start: \(reason)."
        }
    }
}

extension ReviewModel {
    /// Whether a row offers Remove Protection…: a kept finding with exactly
    /// one protection, a marker or a Settings entry, in a report of the
    /// app's roots, while nothing else runs from the window.
    public func canRemoveProtection(_ id: Int) -> Bool {
        guard let row = items?.row(id), row.userEntry == nil, row.finding.rule == ReviewItems.keptRule,
              let target = RemoveProtection.target(row.finding.protection),
              reportScope != .outside, removingProtection == nil, rescanning == nil, !isCleaning, sheet == nil, !updateWaiting()
        else { return false }
        if case .marker = target { return runner != nil }
        return true
    }

    /// The confirmation of a row's Remove Protection…, with the kept findings
    /// it unprotects and, for a marker git tracks, a warning. Nil when the
    /// row offers none.
    public func removalConfirmation(_ id: Int) async -> RemoveProtection.Confirmation? {
        guard canRemoveProtection(id), let items, let row = items.row(id), let target = RemoveProtection.target(row.finding.protection) else {
            return nil
        }
        let findings = items.rows.filter { $0.userEntry == nil && RemoveProtection.holds($0.finding.protection, target) }.map(\.finding)
        var git = GitTracking.Answer.unknown
        if case .marker(let marker) = target {
            git = await gitTracking.marker(in: marker.folder, roots: items.report.scanRoots)
        }
        return RemoveProtection.confirmation(id: id, target: target, findings: findings, git: git, home: items.report.home)
    }

    /// The confirmation's confirm button, and nothing else in the app: takes
    /// the confirmed protection away, then scans again through the window's
    /// `Rescan`, as after a clean. A marker that is gone (`removed`, or
    /// `absent` before) and a Settings entry that is gone both rescan; a
    /// refusal or a failure leaves the protection, and the row says why.
    public func removeProtection(_ confirmed: RemoveProtection.Confirmation) async {
        guard canRemoveProtection(confirmed.id), let items, let row = items.row(confirmed.id),
              RemoveProtection.target(row.finding.protection) == confirmed.target
        else { return }
        let identity = Identity(row.finding)
        protectionMessages[identity] = nil
        removingProtection = RemoveProtection.Progress(id: confirmed.id, stage: .removing)
        let stayed: String?
        switch confirmed.target {
        case .marker(let marker):
            stayed = await removeMarker(marker, home: items.report.home)
        case .settings(let exclusion):
            stayed = removeFromSettings(exclusion)
        }
        if let stayed {
            protectionMessages[identity] = stayed
        }
        guard stayed == nil, !quitting else {
            removingProtection = nil
            return
        }
        removingProtection = RemoveProtection.Progress(id: confirmed.id, stage: .scanning)
        rescanning = .afterRemovingProtection
        await rescan()
        rescanning = nil
        removingProtection = nil
        reload()
    }

    /// Why the row's protection stayed after its Remove Protection…, until
    /// the rows are built again; nil when nothing failed.
    public func protectionMessage(for id: Int) -> String? {
        guard let row = items?.row(id) else { return nil }
        return protectionMessages[Identity(row.finding)]
    }

    /// Nil once the marker is gone; else why it stays.
    private func removeMarker(_ marker: ScanReport.MarkerFolder, home: String) async -> String? {
        guard let runner else { return "The command-line tool is missing, so nothing was removed." }
        log("keep remove requested: \(marker.folder)")
        switch await RemoveProtection.run(runner, marker, log: log) {
        case .report(let report):
            log("keep remove ended: \(report.result)")
            guard !report.result.unprotected else { return nil }
            return RemoveProtection.text(report.result, folder: marker.folder, home: home)
        case .failed(let why):
            return why
        }
    }

    /// The entries of `settings.json` that name the reported folder, in
    /// either list and whatever their spelling, edited through the app's one
    /// writer. Nil once none is left, including when none was there any more.
    private func removeFromSettings(_ exclusion: ScanReport.Exclusion) -> String? {
        var removed: [String] = []
        let written = editSettings { file in
            removed = FolderList.removeEntries(for: exclusion.path, from: &file)
        }
        guard written else {
            return "settings.json could not be changed, so the folder is still protected. Settings → Folders says why."
        }
        log(removed.isEmpty ? "settings: \(exclusion.path) was no longer listed" : "settings: removed \(removed.joined(separator: ", "))")
        return nil
    }
}
