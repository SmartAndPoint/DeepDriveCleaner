/// The plain language of the Review window, the sheet and the result: one
/// table, so every skip reason the executor has reads the same everywhere
/// (phrasing from `reason()` in `crates/ddc-cli/src/prompt.rs`).
public enum CleanWording {
    /// Why an item is or was left alone, as a sentence start.
    public static func reason(_ reason: SkipReason) -> String {
        switch reason {
        case .missing: "Already gone"
        case .notADirectory: "No longer a folder"
        case .symlink: "Replaced by a symlink"
        case .changedSinceScan: "Changed since the scan"
        case .parentChanged: "A folder above it changed since the scan"
        case .ruleMismatch: "No longer matches its rule"
        case .outsideScanRoots: "Outside the scanned folders"
        case .scanRoot: "A scanned folder itself"
        case .protected: "A protected folder"
        case .projectRoot: "A project's own folder"
        case .kept: "Protected by .ddc-keep or an exclusion"
        case .inUse(_, let name): "In use by \(name)"
        case .inUseUnknownCwd(_, let name): "Possibly in use by \(name)"
        case .ownerRunning(_, let name): "\(name) is running"
        case .needsConfirmation: "Needs a confirmation"
        case .actionNotAllowed: "Its rule does not allow this action"
        case .incomplete: "Partly unreadable, or another disk is mounted inside"
        case .nativeUnavailable(let program): "\(program) is not available, or its runtime is not running"
        }
    }

    /// The sheet's live warning: "Google Chrome is running · will be skipped".
    public static func willSkip(_ reason: SkipReason) -> String {
        "\(Self.reason(reason)) · will be skipped"
    }

    /// The result's causes: one per reason, except that running apps merge
    /// into one ("Spotify, Chrome and Brave were running") and processes
    /// holding a folder stay apart ("In use by node").
    public enum Cause: Hashable, Sendable {
        case appsRunning
        case reason(SkipReason)

        public static func of(_ reason: SkipReason) -> Cause {
            switch reason {
            case .ownerRunning: .appsRunning
            // The pid differs per item; the process's name is the cause.
            case .inUse(_, let name): .reason(.inUse(pid: 0, name: name))
            case .inUseUnknownCwd(_, let name): .reason(.inUseUnknownCwd(pid: 0, name: name))
            default: .reason(reason)
            }
        }
    }

    /// "Spotify, Chrome and Brave were running" for `apps`, in their order.
    public static func title(_ cause: Cause, apps: [String]) -> String {
        switch cause {
        case .appsRunning:
            let names = apps.count > 3 ? Array(apps.prefix(3)) + ["\(apps.count - 3) more"] : apps
            return "\(Wording.list(names)) \(apps.count == 1 ? "was" : "were") running"
        case .reason(let reason):
            return Self.reason(reason)
        }
    }

    /// How the sheet names an item's action.
    public static func action(_ action: ListedAction) -> String {
        switch action {
        case .delete: "Delete"
        case .trash: "Move to the Trash"
        case .native: "Clean with its own tool"
        case .nativeOnly: "Run its tool's prune commands"
        }
    }

    public static func note(_ note: ScanReport.Note) -> String {
        switch note {
        case .noLockfile: "No lock file"
        case .appOwned: "Inside an app's own folder"
        case .keptByMarker: "Protected by .ddc-keep"
        case .excluded: "Excluded in Settings"
        }
    }

    /// Why the default selection leaves a finding out.
    public static func notSelected(_ reason: ScanReport.NotSelected) -> String {
        switch reason {
        case .review: "Review · not selected by default"
        case .userData: "Your data · report only"
        case .activeProject(let days): "\(days == 0 ? "Active today" : "Active \(days) \(days == 1 ? "day" : "days") ago") · not selected"
        case .unknownActivity: "Activity unknown · not selected"
        }
    }
}

extension CleanWording {
    /// A row's second line in the Review window: why it is, or is not,
    /// selected, and what the user should know before cleaning it.
    public static func detail(_ row: ReviewItems.Row) -> String {
        var parts: [String] = []
        if let lock = row.lock { parts.append(lock.text) }
        let finding = row.finding
        if let project = finding.project {
            if let days = project.idleDays, finding.notSelected == nil {
                parts.append("Idle \(days) \(days == 1 ? "day" : "days")")
            }
            if project.hasLockfile == true { parts.append("lock file") }
        }
        if row.lock == nil || row.lock == .protected, let reason = finding.notSelected, reason != .userData {
            parts.append(notSelected(reason))
        }
        parts += finding.notes.map(note)
        if finding.incomplete { parts.append("Part of it could not be read; it may be skipped") }
        if finding.class == .review, !finding.hint.isEmpty { parts.append(finding.hint.prefix(1).uppercased() + finding.hint.dropFirst()) }
        return parts.joined(separator: " · ")
    }
}
