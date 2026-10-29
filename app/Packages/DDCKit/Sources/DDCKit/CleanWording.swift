import Foundation

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
        case .nativeUnavailable(let program): "\(program) is not installed"
        case .toolNotReady(_, let detail): detail.prefix(1).uppercased() + detail.dropFirst()
        case .notAnEntry: "No longer a file or folder you can pick in its folder"
        case .entryLocked(let lock): entryLock(lock)
        case .insideAnotherItem: "Went to the Trash with the folder it is in"
        }
    }

    /// Why a file or folder of the user's own folders cannot be picked.
    public static func entryLock(_ lock: EntryLock) -> String {
        switch lock {
        case .appManaged(let app): "\(app) manages it · clean it in \(app)"
        case .kept: "Protected by .ddc-keep or an exclusion"
        case .protected: "Holds a protected folder"
        case .otherVolume: "Another disk is mounted inside"
        case .recent: "Added or changed in the last 10 minutes"
        case .partialDownload: "Still downloading"
        case .unfinishedBackup: "This backup did not finish"
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
        case .userData: "Your data · not selected"
        case .activeProject(let days): "\(days == 0 ? "Active today" : "Active \(days) \(days == 1 ? "day" : "days") ago") · not selected"
        case .unknownActivity: "Activity unknown · not selected"
        }
    }
}

extension CleanWording {
    public static func className(_ riskClass: ScanReport.RiskClass) -> String {
        switch riskClass {
        case .safe: "Safe"
        case .rebuildable: "Rebuildable"
        case .review: "Review"
        case .userData: "User data"
        }
    }

    /// What a class means, at the top of each of its groups.
    public static func classBlurb(_ riskClass: ScanReport.RiskClass) -> String {
        switch riskClass {
        case .safe: "Caches that refill on the next install or build. Cleaned with the tool's own command when it has one."
        case .rebuildable: "Environments and build folders of projects untouched for a while that have a lock file."
        case .review: "Expensive to restore, or restorability is uncertain. Not selected by default."
        case .userData: "Your own files. Nothing here is ever selected for you."
        }
    }

    /// The sidebar's name for a user-data rule: one entry per rule. Nil for
    /// a rule this app does not know, which keeps its group's name.
    public static func userDataName(_ rule: String) -> String? {
        switch rule {
        case "user.downloads": "Downloads"
        case "user.pictures": "Pictures"
        case "user.movies": "Movies"
        case "user.music": "Music"
        case "user.ios-backups": "iPhone and iPad backups"
        case "user.imazing-backups": "iMazing backups"
        case "user.telegram": "Telegram"
        case "user.whatsapp": "WhatsApp"
        case "user.cloud": cloudFolders
        case "user.kept": "Protected by you"
        case "xcode.archives": "Xcode archives"
        case "ide.workspace-storage": "Editor workspace storage"
        case "macos.app-bundle": "Apps"
        case "node.global-packages": "Global packages"
        default: nil
        }
    }

    /// The sidebar's last User data entry: the folders the scan never enters.
    public static let cloudFolders = "Cloud folders"

    /// What the Trash's group says at its top (story "The Trash: its own row,
    /// not Your data"). DeepDriveCleaner never empties it (safety ADR §1).
    public static let trashBlurb = "What was moved to the Trash, by you or by a clean. Its space comes back when you empty the Trash in Finder."

    /// What a user-data group with entries says once, at its top.
    public static let pickBlurb = "Pick files and folders one by one to move them to the Trash. Put Back in the Trash restores them."

    /// Why the cloud folders are not scanned.
    public static let cloudBanner = "iCloud Drive and other cloud folders are not scanned: a file there may live only in the cloud, and the Trash would remove it from every device. To free space, use Remove Download in Finder."

    /// A background scan skipped the folders macOS guards with a prompt.
    public static let backgroundSkipped = "A background scan skipped Downloads, Pictures and other protected folders. Scan Now to list them."

    /// The user's skips: "Not scanned: ~/Projects/client-archive and ~/Movies/Raw footage (Skip entirely in Settings)".
    public static func notScanned(_ folders: [String]) -> String {
        "Not scanned: \(Wording.list(folders)) (Skip entirely in Settings)"
    }

    /// "1,904 smaller items (2.3 GiB) are not listed".
    public static func rest(count: UInt64, bytes: UInt64) -> String {
        let noun = count == 1 ? "smaller item" : "smaller items"
        return "\(Wording.number(Int(clamping: count))) \(noun) (\(ByteSize.human(bytes))) \(count == 1 ? "is" : "are") not listed"
    }

    /// An entry row's own facts: its lock, when it last changed, a backup's
    /// device and date, the findings inside it, `.git`.
    public static func entryDetail(_ row: ReviewItems.Row, scanned: Date?) -> String {
        guard let entry = row.userEntry else { return "" }
        var parts: [String] = []
        if let lock = row.lock { parts.append(lock.text) }
        if let backup = entry.backup {
            if let name = backup.deviceName { parts.append(Wording.escaped(name)) }
            if let date = backup.lastBackup.flatMap(Wording.date(rfc3339:)) {
                parts.append("Last backup \(date.formatted(.dateTime.year().month().day()))")
            }
        } else if row.lock != .entry(.recent), let scanned, let modified = entry.modified.flatMap(Wording.date(rfc3339:)) {
            let days = max(0, Int(scanned.timeIntervalSince(modified) / 86_400))
            parts.append(days == 0 ? "Last changed today" : "Last changed \(days) \(days == 1 ? "day" : "days") ago")
        }
        if entry.findings > 0 {
            parts.append("Holds \(Wording.items(Int(clamping: entry.findings), "finding")) of its own")
        }
        if entry.git { parts.append("A project (has .git)") }
        if row.finding.incomplete { parts.append("Partly unreadable") }
        return parts.joined(separator: " · ")
    }

    /// A finding's way back, as a sentence; nil when it has none.
    public static func hint(_ hint: String) -> String? {
        hint.isEmpty ? nil : hint.prefix(1).uppercased() + hint.dropFirst()
    }

    /// What the clean does with rows of one rule: their action, else why none.
    public static func actionText(_ rows: [ReviewItems.Row]) -> String {
        if let action = rows.lazy.compactMap(\.action).first { return Self.action(action) }
        return rows.contains { $0.lock == .noAction } ? ReviewItems.Lock.noAction.text : "Report only"
    }

    /// A row's own facts in the Groups view: its rule, action, class and way
    /// back are said once on its heading, and so are the notes in `shared`.
    public static func ownDetail(_ row: ReviewItems.Row, omitting shared: Set<ScanReport.Note>) -> String {
        var parts: [String] = []
        if let lock = ownLock(row) { parts.append(lock.text) }
        let finding = row.finding
        // Board 9 shows it on review rows too; an active project says "Active" instead.
        if let days = finding.project?.idleDays {
            if case .activeProject? = finding.notSelected {} else { parts.append("Untouched \(days) \(days == 1 ? "day" : "days")") }
        }
        if row.lock == nil || row.lock == .protected, let reason = finding.notSelected {
            switch reason {
            case .activeProject, .unknownActivity: parts.append(notSelected(reason))
            case .review, .userData: break
            }
        }
        parts += finding.notes.filter { !shared.contains($0) }.map(note)
        if finding.incomplete { parts.append("Part of it could not be read; it may be skipped") }
        return parts.joined(separator: " · ")
    }

    /// The lock a row states in the Groups view: user data and "no command"
    /// are said once, by the class and the heading.
    public static func ownLock(_ row: ReviewItems.Row) -> ReviewItems.Lock? {
        guard let lock = row.lock, lock != .userData, lock != .noAction else { return nil }
        return lock
    }

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
