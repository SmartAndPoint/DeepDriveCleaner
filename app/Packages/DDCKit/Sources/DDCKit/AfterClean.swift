import Foundation

/// What a skip means for the next clean: the table of story "Review window:
/// after a clean, only skips that can clear stay selected". The sheet's
/// advice and the result read it, so they never advise differently for the
/// same item. In this order the sheet's warnings line names them.
public enum AfterClean: Sendable, Hashable, Comparable {
    /// Can clear by itself or by one step of the user's: stays selected.
    case keep
    /// The CLI re-checks against the last scan, so only a new scan clears it.
    case rescan
    /// Never cleanable from here.
    case never
}

extension SkipReason {
    /// No `default:`: a new reason cannot be added without choosing.
    public var afterClean: AfterClean {
        switch self {
        // `agentLive`: quitting the agent clears it.
        case .ownerRunning, .inUse, .inUseUnknownCwd, .toolNotReady, .agentLive:
            .keep
        // `incomplete`: the walk was partial; nothing in it says a permission would fix it.
        case .changedSinceScan, .parentChanged, .missing, .ruleMismatch, .notAnEntry, .entryLocked, .symlink, .notADirectory,
             .incomplete:
            .rescan
        case .kept, .protected, .projectRoot, .scanRoot, .outsideScanRoots, .actionNotAllowed, .needsConfirmation,
             .insideAnotherItem, .nativeUnavailable:
            .never
        }
    }
}

extension CleanReport.Outcome {
    /// Whether the item stays selected after the clean: a skip that can
    /// clear, or an item a stopped clean did not reach. A tool that ran,
    /// succeeded or not, would run again the same way; a failure would fail again.
    public var staysSelected: Bool {
        switch self {
        case .skipped(let reason): reason.afterClean == .keep
        case .notRun: true
        case .deleted, .trashed, .native, .failed: false
        }
    }
}

extension CleanWording {
    /// What the result says under its causes: what clears the skips that
    /// stay selected, then the items only a new scan clears, as the sheet's
    /// `advice` says it. Nil when neither.
    public static func resultAdvice(_ result: CleanResult) -> String? {
        var sentences = clearing(result.canClear, again: "clean again")
        if result.needsRescan > 0 { sentences.append(changedSinceScan(Wording.items(result.needsRescan), count: result.needsRescan)) }
        return sentences.isEmpty ? nil : sentences.joined(separator: " ")
    }

    /// "2.4 GiB went to the Trash. It comes back when you empty the Trash.";
    /// nil when nothing went there. A move to the Trash frees nothing yet.
    public static func trashed(_ result: CleanResult) -> String? {
        guard result.trashed.count > 0 else { return nil }
        return "\(ByteSize.human(result.trashed.bytes)) went to the Trash. It comes back when you empty the Trash."
    }

    /// The free-space note, only when something was deleted or a tool ran:
    /// nothing else frees space now.
    public static func apfsNote(_ result: CleanResult) -> String? {
        result.deleted.count + result.toolsRun.count > 0 ? "APFS may take a moment to give the space back." : nil
    }

    /// The result's last line: "6 items stay selected for the next clean.";
    /// nil when none does.
    public static func staysSelected(_ result: CleanResult) -> String? {
        let count = result.staysSelected
        guard count > 0 else { return nil }
        return "\(Wording.items(count)) \(count == 1 ? "stays" : "stay") selected for the next clean."
    }

    /// A row's note after its tool ran in the window's last clean: "Pruned 3
    /// min ago; Docker Desktop shrinks its disk image later", or "The prune
    /// failed: <what it said>". Nil when its tool did not run.
    public static func toolNote(_ item: CleanReport.Item, ended: Date, now: Date) -> String? {
        guard case .native(let commands) = item.outcome else { return nil }
        let prune = item.action == .nativeOnly
        if let failed = commands.first(where: { !$0.success }) {
            return "\(prune ? "The prune" : "Its tool") failed\(CleanResult.said(failed))"
        }
        let ago = Wording.age(of: ended, now: now)
        guard prune else { return "Cleaned by its tool \(ago)" }
        // Docker frees the space inside Docker.raw; the file shrinks later (safety ADR §1).
        let docker = commands.contains { URL(filePath: $0.program).lastPathComponent == "docker" }
        return "Pruned \(ago)" + (docker ? "; Docker Desktop shrinks its disk image later" : "")
    }
}
