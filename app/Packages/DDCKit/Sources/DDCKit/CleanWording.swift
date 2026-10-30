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
        case .excluded: excluded(nil)
        }
    }

    /// A folder excluded itself, or under an exclusion, by its origin; a
    /// report that does not say names both.
    public static func excluded(_ origin: ScanReport.ExclusionOrigin?) -> String {
        switch origin {
        case .settings: "Excluded in Settings"
        case .flag: "Excluded by a command-line flag"
        case nil: "Excluded in Settings or by a command-line flag"
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

    /// An entry row's own facts: its lock, a backup's device and date, the
    /// findings inside it, `.git`. When it last changed is its Changed column.
    public static func entryDetail(_ row: ReviewItems.Row) -> String {
        guard let entry = row.userEntry else { return "" }
        var parts: [String] = []
        if let lock = row.lock { parts.append(lock.text) }
        if let backup = entry.backup {
            if let name = backup.deviceName { parts.append(Wording.escaped(name)) }
            if let date = backup.lastBackup.flatMap(Wording.date(rfc3339:)) {
                parts.append("Last backup \(date.formatted(.dateTime.year().month().day()))")
            }
        }
        if entry.findings > 0 {
            parts.append("Holds \(Wording.items(Int(clamping: entry.findings), "finding")) of its own")
        }
        if entry.git { parts.append("A project (has .git)") }
        if row.finding.incomplete { parts.append("Partly unreadable") }
        return parts.joined(separator: " · ")
    }

    /// Rules whose hint a row says whatever its class: conda's package cache
    /// frees only the packages no environment uses, so less than its size
    /// (story "Whole disk: Conda environments and its package cache, with conda").
    public static let hintedThoughSafe: Set<String> = ["cache.conda"]

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
    /// `home` shortens the paths a protection names; without it they show in full.
    public static func ownDetail(_ row: ReviewItems.Row, omitting shared: Set<ScanReport.Note>, home: String? = nil) -> String {
        var parts: [String] = []
        if let lock = ownLock(row) { parts.append(lock.text) }
        let finding = row.finding
        // Board 9 shows it on review rows too; an active project says "Active" instead.
        if let days = finding.project?.idleDays {
            if case .activeProject? = finding.notSelected {} else { parts.append(projectUntouched(days)) }
        }
        if row.lock == nil || row.lock == .protected, let reason = finding.notSelected {
            switch reason {
            case .activeProject, .unknownActivity: parts.append(notSelected(reason))
            case .review, .userData: break
            }
        }
        parts += statedNotes(finding).filter { !shared.contains($0) }.map(note)
        parts += protection(finding, home: home)
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
    /// selected, and what the user should know before cleaning it. `home`
    /// shortens the paths a protection names; without it they show in full.
    public static func detail(_ row: ReviewItems.Row, home: String? = nil) -> String {
        var parts: [String] = []
        if let lock = row.lock { parts.append(lock.text) }
        let finding = row.finding
        if let project = finding.project {
            if let days = project.idleDays, finding.notSelected == nil {
                parts.append(projectUntouched(days))
            }
            if project.hasLockfile == true { parts.append("lock file") }
        }
        if row.lock == nil || row.lock == .protected, let reason = finding.notSelected, reason != .userData {
            parts.append(notSelected(reason))
        }
        parts += statedNotes(finding).map(note)
        parts += protection(finding, home: home)
        if finding.incomplete { parts.append("Part of it could not be read; it may be skipped") }
        if finding.class == .review || Self.hintedThoughSafe.contains(finding.rule), let hint = Self.hint(finding.hint) { parts.append(hint) }
        return parts.joined(separator: " · ")
    }
}

// MARK: - Protected by you

/// Story "Protected by you: say what it is, and Remove Protection": what a
/// kept finding is protected by, in the row's second line, and what the
/// group says at its top.
extension CleanWording {
    /// The Protected by you group's blurb.
    public static let keptBlurb = "Folders you protected with a .ddc-keep file, an exclusion or a skip. DeepDriveCleaner never removes them. Remove the protection to clean one."

    /// The User data section's blurb in the sidebar: only a folder that
    /// lists its files has any to pick.
    public static let userDataSectionBlurb = "Your own files. Nothing is selected for you. Where a folder lists its files, pick them one by one and they go to the Trash."

    /// A row whose only protections are command-line flags.
    public static let flagOnly = "The next scan without the flag lists this folder"

    /// A row with several protections: Remove Protection… is not offered.
    public static let severalProtections = "Remove them in Finder or in Settings → Folders"

    /// The notes a row says as notes. A finding that says what protects it
    /// says it in its own words (`protection`), never as `.keptByMarker` or
    /// `.excluded`, which cannot tell a flag from Settings.
    public static func statedNotes(_ finding: ScanReport.Finding) -> [ScanReport.Note] {
        guard finding.protection != nil else { return finding.notes }
        return finding.notes.filter { $0 != .keptByMarker && $0 != .excluded }
    }

    /// What protects a kept finding, one part per protection, then the rule
    /// that would claim it ("otherwise: node_modules"), then how to remove
    /// them when Remove Protection… cannot. Empty without `protection`.
    public static func protection(_ finding: ScanReport.Finding, home: String?) -> [String] {
        guard let protection = finding.protection else { return [] }
        let shown = { (path: String) in Wording.tilde(path, home: home ?? "") }
        var parts = protection.markers.map { "Protected by .ddc-keep in \(shown($0.folder))" }
        parts += protection.exclusions.map { exclusion in
            guard exclusion.inside else { return excluded(exclusion.origin) }
            return switch (exclusion.kind, exclusion.origin) {
            case (.skip, .settings): "Holds a folder skipped in Settings: \(shown(exclusion.path))"
            case (.skip, .flag): "Holds a folder skipped by a command-line flag: \(shown(exclusion.path))"
            case (.exclude, _): "Holds an excluded folder: \(shown(exclusion.path))"
            }
        }
        if let rule = protection.rule { parts.append("otherwise: \(ruleName(rule))") }
        // A flag is not in Settings: a row it protects is never sent there.
        if protection.exclusions.contains(where: { $0.origin == .flag }) {
            parts.append(flagOnly)
        } else if protection.markers.count + protection.exclusions.count > 1 {
            parts.append(severalProtections)
        }
        return parts
    }

    /// A rule as a row names it: its whole id. Its last part alone is
    /// shared by several rules (`rust.target`, `maven.target`), so
    /// "otherwise: target" would not say which.
    public static func ruleName(_ rule: String) -> String {
        Wording.escaped(rule)
    }
}

// MARK: - The Changed column and the sort header

/// Story "Review window: sort a group's rows by name, size or last change".
/// Dates are read against the scan's time, the report's `generated_at`, as
/// the second line measures days; the calendar, locale and time zone are
/// passed in so tests fix them.
extension CleanWording {
    /// The project's activity on a row's second line: a signal of its own,
    /// apart from the row's Changed column.
    public static func projectUntouched(_ days: UInt64) -> String {
        "Project untouched \(days) \(days == 1 ? "day" : "days")"
    }

    /// The Changed column of a row with no date.
    public static let noDate = "—"

    /// The Changed column: "Today" or "Yesterday" as of the scan, else an
    /// abbreviated date ("Sep 25, 2026"); "—" with no date. A time after
    /// the scan (a wrong clock) shows as it is.
    public static func changed(
        _ modified: String?,
        scanned: Date?,
        calendar: Calendar = .autoupdatingCurrent,
        locale: Locale = .autoupdatingCurrent,
        timeZone: TimeZone = .autoupdatingCurrent
    ) -> String {
        guard let date = modified.flatMap(Wording.date(rfc3339:)) else { return noDate }
        var calendar = calendar
        calendar.timeZone = timeZone
        if let scanned {
            if calendar.isDate(date, inSameDayAs: scanned) { return "Today" }
            if let yesterday = calendar.date(byAdding: .day, value: -1, to: scanned), calendar.isDate(date, inSameDayAs: yesterday) {
                return "Yesterday"
            }
        }
        return date.formatted(Date.FormatStyle(date: .abbreviated, time: .omitted, locale: locale, calendar: calendar, timeZone: timeZone))
    }

    /// A row's Changed tooltip; nil with no date. A finding's date is the
    /// newest change anywhere inside it, a lower bound when part of it could
    /// not be read; an entry of the user's own folders has its own change.
    public static func changedHelp(
        _ row: ReviewItems.Row,
        calendar: Calendar = .autoupdatingCurrent,
        locale: Locale = .autoupdatingCurrent,
        timeZone: TimeZone = .autoupdatingCurrent
    ) -> String? {
        guard let text = dateAndTime(row.finding.modified, calendar: calendar, locale: locale, timeZone: timeZone) else { return nil }
        if row.userEntry != nil { return "Changed: \(text)" }
        return row.finding.incomplete
            ? "Newest change inside: at least as recent as \(text) (part of it could not be read)"
            : "Newest change inside: \(text)"
    }

    /// A subgroup heading's Changed tooltip; nil with no date.
    public static func subgroupChangedHelp(
        _ modified: String?,
        calendar: Calendar = .autoupdatingCurrent,
        locale: Locale = .autoupdatingCurrent,
        timeZone: TimeZone = .autoupdatingCurrent
    ) -> String? {
        dateAndTime(modified, calendar: calendar, locale: locale, timeZone: timeZone).map { "Newest change among them: \($0)" }
    }

    private static func dateAndTime(_ modified: String?, calendar: Calendar, locale: Locale, timeZone: TimeZone) -> String? {
        guard let date = modified.flatMap(Wording.date(rfc3339:)) else { return nil }
        var calendar = calendar
        calendar.timeZone = timeZone
        return date.formatted(Date.FormatStyle(date: .abbreviated, time: .shortened, locale: locale, calendar: calendar, timeZone: timeZone))
    }

    /// A column's title in the sort header.
    public static func sortTitle(_ column: ReviewSort.Column) -> String {
        switch column {
        case .name: "Name"
        case .size: "Size"
        case .changed: "Changed"
        }
    }

    /// Which way a column is sorted: "oldest first".
    static func sortDirection(_ sort: ReviewSort) -> String {
        switch sort.column {
        case .name: sort.ascending ? "A to Z" : "Z to A"
        case .size: sort.ascending ? "smallest first" : "largest first"
        case .changed: sort.ascending ? "oldest first" : "newest first"
        }
    }

    /// A column title's accessibility label: "Changed, sorted oldest first"
    /// on the sorted column, its title alone on the others.
    public static func sortLabel(_ column: ReviewSort.Column, sort: ReviewSort) -> String {
        column == sort.column ? "\(sortTitle(column)), sorted \(sortDirection(sort))" : sortTitle(column)
    }

    /// What pressing a column title does.
    public static func sortHint(_ column: ReviewSort.Column, sort: ReviewSort) -> String {
        column == sort.column ? "Reverses the order" : "Sorts by \(sortTitle(column).lowercased())"
    }

    /// The Items view's heading, which names the sort: "Items · oldest change first".
    public static func itemsHeading(_ sort: ReviewSort) -> String {
        let order =
            switch sort.column {
            case .name: "by name, \(sortDirection(sort))"
            case .size: sortDirection(sort)
            case .changed: sort.ascending ? "oldest change first" : "newest change first"
            }
        return "Items · \(order)"
    }
}
