/// What the sheet's dry run predicts the clean will skip (story "Review
/// window: Clean is not offered when every item would be skipped"): the
/// header leaves these items out of "frees about", and when every item
/// would be skipped the sheet offers no clean.
public struct SkipSummary: Sendable, Equatable {
    /// The confirmed items predicted to be skipped.
    public let ids: Set<Int>
    /// Their reasons, in the sheet's order.
    public let reasons: [SkipReason]
    /// What the scan measured of them.
    public let bytes: UInt64
    /// The check finished and every confirmed item would be skipped.
    public let nothingToClean: Bool

    init(confirmation: Confirmation, predictions: [Int: Prediction], checked: Bool) {
        var ids: Set<Int> = []
        var reasons: [SkipReason] = []
        var bytes: UInt64 = 0
        for item in confirmation.items {
            guard case .wouldSkip(let reason)? = predictions[item.id] else { continue }
            ids.insert(item.id)
            reasons.append(reason)
            bytes += item.finding.bytes
        }
        self.ids = ids
        self.reasons = reasons
        self.bytes = bytes
        nothingToClean = checked && !confirmation.items.isEmpty && ids.count == confirmation.items.count
    }

    public var count: Int { reasons.count }

    /// The running apps, once each, in the order first seen.
    public var apps: [String] {
        var apps: [String] = []
        for case .ownerRunning(_, let name) in reasons where !apps.contains(name) { apps.append(name) }
        return apps
    }

    /// The classes of `afterClean` present, in the order the line names them.
    public var classes: [AfterClean] { Set(reasons.map(\.afterClean)).sorted() }
}

extension ReviewModel.Sheet {
    public var skipSummary: SkipSummary {
        SkipSummary(confirmation: confirmation, predictions: predictions, checked: warnings == .checked)
    }

    /// Whether the Clean button is on, and `cleanConfirmedItems()` runs: not
    /// while the check runs, nor when it said every item would be skipped.
    /// Waiting for a scan, failed, stopped or busy leaves it on: the CLI
    /// re-checks every item before it touches it (safety ADR §7).
    public var cleanAllowed: Bool {
        stage == .confirm && warnings != .checking && !skipSummary.nothingToClean
    }
}

extension CleanWording {
    /// The sheet's title: "Move 2 items to the Trash?" when every item goes
    /// there (story "User data: move a folder's picked files to the Trash
    /// from that folder"), otherwise "Clean 2 items?".
    public static func sheetTitle(_ confirmation: Confirmation) -> String {
        let items = Wording.items(confirmation.items.count)
        return confirmation.movesOnlyToTrash ? "Move \(items) to the Trash?" : "Clean \(items)?"
    }

    /// The sheet's button, worded as its title.
    public static func sheetAction(_ confirmation: Confirmation) -> String {
        confirmation.movesOnlyToTrash ? "Move to the Trash" : "Clean \(Wording.items(confirmation.items.count))"
    }

    /// The sheet's header: "1.6 GiB selected · frees about 0.4 GiB; 1.2 GiB
    /// will be skipped", or "… frees about 1.6 GiB before the check" while
    /// the check waits for a scan.
    public static func sheetHeader(_ sheet: ReviewModel.Sheet) -> String {
        let confirmation = sheet.confirmation
        let skips = sheet.skipSummary
        var text = "\(ByteSize.human(confirmation.bytes)) selected · frees about \(ByteSize.human(confirmation.reclaimable(excluding: skips.ids)))"
        if sheet.warnings == .waiting { text += " before the check" }
        if skips.count > 0 { text += "; \(ByteSize.human(skips.bytes)) will be skipped" }
        return text
    }

    /// The warnings line when every item would be skipped, with what to do.
    public static func nothingToClean(_ skips: SkipSummary) -> String {
        "Nothing can be cleaned right now: every item would be skipped. " + advice(skips)
    }

    /// What to do about the predicted skips, by their class in `AfterClean`:
    /// the skips that can clear, then those that need a scan, then the causes
    /// that never clear here. DeepDriveCleaner never quits an app itself.
    public static func advice(_ skips: SkipSummary) -> String {
        let reasons = skips.reasons
        var sentences = clearing(reasons, again: "Check Again")
        // Needs a new scan: Check Again would skip them again.
        let rescan = reasons.filter { $0.afterClean == .rescan }.count
        if rescan > 0 { sentences.append(changedSinceScan(rescan == skips.count ? "These items" : Wording.items(rescan), count: rescan)) }
        // Never here: the cause, as the result names it.
        var causes: [String] = []
        for reason in reasons where reason.afterClean == .never {
            let cause = Self.reason(reason)
            if !causes.contains(cause) { causes.append(cause) }
        }
        sentences += causes.map { "\($0)." }
        return sentences.joined(separator: " ")
    }

    /// What clears the skips of `reasons` that can clear: the running apps,
    /// the processes holding a folder, the tools not ready. `again` is what
    /// to do then: "Check Again" in the sheet, "clean again" in the result.
    static func clearing(_ reasons: [SkipReason], again: String) -> [String] {
        var sentences: [String] = []
        var apps: [String] = []
        for case .ownerRunning(_, let name) in reasons where !apps.contains(name) { apps.append(name) }
        if !apps.isEmpty { sentences.append("Quit \(Wording.list(apps)), then \(again).") }
        // The processes known to hold a folder first, then the possible ones.
        var certain: [String] = []
        var possible: [String] = []
        for reason in reasons {
            switch reason {
            case .inUse(_, let name):
                if !certain.contains(name) { certain.append(name) }
            case .inUseUnknownCwd(_, let name):
                if !possible.contains(name) { possible.append(name) }
            default:
                break
            }
        }
        possible.removeAll { certain.contains($0) }
        for (lead, processes) in [("In use by", certain), ("Possibly in use by", possible)] where !processes.isEmpty {
            let ends = processes.count == 1 ? "it ends" : "they end"
            sentences.append("\(lead) \(Wording.list(processes)); \(again) when \(ends).")
        }
        var details: [String] = []
        for case .toolNotReady(_, let detail) in reasons where !details.contains(detail) { details.append(detail) }
        sentences += details.map { toolNotReady($0, again: again) }
        if details.contains(where: { $0 == dockerDown || $0 == dockerNoContext }) {
            sentences.append("If you no longer use Docker Desktop, its folder is yours to delete in Finder.")
        }
        return sentences
    }

    /// "2 items changed since the scan. Scan Again to see them as they are now."
    static func changedSinceScan(_ which: String, count: Int) -> String {
        "\(which) changed since the scan. Scan Again to see \(count == 1 ? "it as it is" : "them as they are") now."
    }

    /// `DOCKER_DOWN` and the missing context of `crates/ddc-core/src/rules/catalog.rs`.
    static let dockerDown = "Docker Desktop is not running or not answering"
    static let dockerNoContext = "the desktop-linux context does not exist"

    /// What to do about a tool that did not answer, by the CLI's detail.
    public static func toolNotReady(_ detail: String, again: String = "Check Again") -> String {
        switch detail {
        case dockerDown: "Start Docker Desktop, then \(again)."
        case dockerNoContext: "The desktop-linux context is missing; open Docker Desktop once to create it, then \(again)."
        default: "\(reason(.toolNotReady(program: "", detail: detail))). \(again.prefix(1).uppercased() + again.dropFirst()) when it is ready."
        }
    }
}
