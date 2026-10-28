import Foundation
import Testing
@testable import DDCKit

/// Story "Review window: Clean is not offered when every item would be
/// skipped": the sheet's summary of the predicted skips, whether Clean is
/// on, and what the header and the warnings line say. Pure: the sheet is
/// built from `ReviewFixture`, with no view and no CLI.
@Suite("Review window: predicted skips")
struct SkipSummaryTests {
    let confirmation: DDCKit.Confirmation

    init() throws {
        let items = try ReviewFixture.items()
        confirmation = DDCKit.Confirmation(items, selection: try ReviewFixture.recordedSelection(items), deletePermanently: false)
    }

    /// The sheet at `warnings`, each item with the prediction `predict` gives it.
    private func sheet(_ warnings: ReviewModel.Warnings, _ predict: (Int, DDCKit.Confirmation.Item) -> Prediction?) -> ReviewModel.Sheet {
        var predictions: [Int: Prediction] = [:]
        for (index, item) in confirmation.items.enumerated() { predictions[item.id] = predict(index, item) }
        return ReviewModel.Sheet(confirmation: confirmation, warnings: warnings, predictions: predictions, stage: .confirm, progress: nil)
    }

    private func every(_ reason: SkipReason) -> (Int, DDCKit.Confirmation.Item) -> Prediction? {
        { _, _ in .wouldSkip(reason) }
    }

    private static let chrome = SkipReason.ownerRunning(pid: 4242, name: "Google Chrome")
    private static let code = SkipReason.ownerRunning(pid: 77, name: "Visual Studio Code")

    @Test("every item skipped for a running app: nothing to clean, Clean off, and the line names each app once in the order first seen")
    func allOwnerRunning() {
        // Code first, then Chrome, then both again.
        let sheet = sheet(.checked) { index, _ in .wouldSkip(index % 2 == 0 ? Self.code : Self.chrome) }
        #expect(confirmation.items.count == 6)
        let skips = sheet.skipSummary
        #expect(skips.nothingToClean)
        #expect(skips.count == 6 && skips.bytes == confirmation.bytes)
        #expect(skips.apps == ["Visual Studio Code", "Google Chrome"])
        #expect(skips.classes == [.keep])
        #expect(!sheet.cleanAllowed)
        #expect(CleanWording.advice(skips) == "Quit Visual Studio Code and Google Chrome, then Check Again.")
        #expect(CleanWording.nothingToClean(skips)
            == "Nothing can be cleaned right now: every item would be skipped. Quit Visual Studio Code and Google Chrome, then Check Again.")
        #expect(CleanWording.sheetHeader(sheet)
            == "\(ByteSize.human(confirmation.bytes)) selected · frees about 0 B; \(ByteSize.human(confirmation.bytes)) will be skipped")
    }

    @Test("2 of 6 predicted to skip: frees about counts the other 4, the skipped bytes are named, and Clean stays on")
    func twoOfSix() throws {
        let skipped = Set(confirmation.items.filter { $0.finding.path.hasSuffix("Google/Chrome") || $0.finding.path.hasSuffix("r1/target") }.map(\.id))
        #expect(skipped.count == 2)
        let sheet = sheet(.checked) { _, item in skipped.contains(item.id) ? .wouldSkip(Self.chrome) : .wouldDelete }
        let others = confirmation.items.filter { !skipped.contains($0.id) }
        let frees = others.reduce(UInt64(0)) { $0 + $1.finding.reclaimableBytes }
        let skippedBytes = confirmation.items.filter { skipped.contains($0.id) }.reduce(UInt64(0)) { $0 + $1.finding.bytes }
        #expect(others.count == 4)
        #expect(frees < confirmation.reclaimable)

        let skips = sheet.skipSummary
        #expect(skips.ids == skipped && skips.count == 2 && skips.bytes == skippedBytes)
        #expect(!skips.nothingToClean)
        #expect(sheet.cleanAllowed)
        #expect(confirmation.reclaimable(excluding: skips.ids) == frees)
        #expect(CleanWording.sheetHeader(sheet)
            == "\(ByteSize.human(confirmation.bytes)) selected · frees about \(ByteSize.human(frees)); \(ByteSize.human(skippedBytes)) will be skipped")
    }

    @Test("checking: Clean off; waiting: Clean on, with \"before the check\"; the check then finding every item skipped turns it off")
    func checkStates() {
        let checking = sheet(.checking) { _, _ in nil }
        #expect(!checking.cleanAllowed)
        // Lines already in for every item: still off until the check ends, and not yet "nothing to clean".
        let allIn = sheet(.checking, every(Self.chrome))
        #expect(!allIn.cleanAllowed && !allIn.skipSummary.nothingToClean)

        var sheet = sheet(.waiting) { _, _ in nil }
        #expect(sheet.cleanAllowed)
        #expect(!sheet.skipSummary.nothingToClean)
        #expect(CleanWording.sheetHeader(sheet)
            == "\(ByteSize.human(confirmation.bytes)) selected · frees about \(ByteSize.human(confirmation.reclaimable)) before the check")

        for item in confirmation.items { sheet.predictions[item.id] = .wouldSkip(Self.chrome) }
        sheet.warnings = .checked
        #expect(sheet.skipSummary.nothingToClean)
        #expect(!sheet.cleanAllowed)
        #expect(!CleanWording.sheetHeader(sheet).contains("before the check"))
    }

    @Test("a check that failed, was stopped or found the lock held leaves Clean on, even with every line a skip")
    func otherStates() {
        for warnings in [ReviewModel.Warnings.failed("Checking for warnings failed: exit 1."), .stopped, .busy] {
            let sheet = sheet(warnings, every(Self.chrome))
            #expect(!sheet.skipSummary.nothingToClean, "\(warnings)")
            #expect(sheet.cleanAllowed, "\(warnings)")
        }
    }

    @Test("Clean is off outside the confirm stage")
    func otherStages() {
        var sheet = sheet(.checked) { _, _ in .wouldDelete }
        #expect(sheet.cleanAllowed)
        sheet.stage = .cleaning(.waiting)
        #expect(!sheet.cleanAllowed)
    }

    @Test("every item changed since the scan: Scan Again, not Check Again; with running apps too, both, apps first")
    func changedSinceScan() {
        let changed = sheet(.checked, every(.changedSinceScan)).skipSummary
        #expect(changed.nothingToClean && changed.apps.isEmpty && changed.classes == [.rescan])
        #expect(CleanWording.advice(changed) == "These items changed since the scan. Scan Again to see them as they are now.")
        #expect(!CleanWording.advice(changed).contains("Check Again"))

        // The changed items come first in the sheet; the apps still lead the line.
        let mixed = sheet(.checked) { index, _ in .wouldSkip(index < 2 ? .changedSinceScan : Self.chrome) }.skipSummary
        #expect(mixed.nothingToClean && mixed.classes == [.keep, .rescan])
        #expect(CleanWording.advice(mixed)
            == "Quit Google Chrome, then Check Again. 2 items changed since the scan. Scan Again to see them as they are now.")
    }

    @Test("a process holding a folder, a tool not ready and a cause that never clears each read as what to do, in the table's order")
    func otherAdvice() {
        let held = sheet(.checked) { index, _ in .wouldSkip(index < 3 ? .inUse(pid: 1, name: "node") : .inUse(pid: 2, name: "node")) }.skipSummary
        #expect(CleanWording.advice(held) == "In use by node; Check Again when it ends.")
        let maybe = sheet(.checked) { index, _ in .wouldSkip(.inUseUnknownCwd(pid: Int64(index), name: index < 3 ? "node" : "python")) }.skipSummary
        #expect(CleanWording.advice(maybe) == "Possibly in use by node and python; Check Again when they end.")
        let both = sheet(.checked) { index, _ in
            .wouldSkip(index < 3 ? .inUse(pid: 1, name: "node") : .inUseUnknownCwd(pid: 2, name: "python"))
        }.skipSummary
        #expect(CleanWording.advice(both)
            == "In use by node; Check Again when it ends. Possibly in use by python; Check Again when it ends.")

        let footer = "If you no longer use Docker Desktop, its folder is yours to delete in Finder."
        let down = sheet(.checked, every(.toolNotReady(program: "docker", detail: CleanWording.dockerDown))).skipSummary
        #expect(CleanWording.advice(down) == "Start Docker Desktop, then Check Again. \(footer)")
        let context = sheet(.checked, every(.toolNotReady(program: "docker", detail: CleanWording.dockerNoContext))).skipSummary
        #expect(CleanWording.advice(context)
            == "The desktop-linux context is missing; open Docker Desktop once to create it, then Check Again. \(footer)")
        let other = sheet(.checked, every(.toolNotReady(program: "podman", detail: "the machine is stopped"))).skipSummary
        #expect(CleanWording.advice(other) == "The machine is stopped. Check Again when it is ready.")

        let never = sheet(.checked) { index, _ in .wouldSkip(index < 3 ? .protected : .nativeUnavailable(program: "go")) }.skipSummary
        #expect(never.classes == [.never])
        #expect(CleanWording.advice(never) == "A protected folder. go is not installed.")

        // keep → rescan → never, whatever the sheet's order.
        let all: [SkipReason] = [.kept, .missing, Self.chrome, .kept, .symlink, .toolNotReady(program: "docker", detail: CleanWording.dockerDown)]
        let mix = sheet(.checked) { index, _ in .wouldSkip(all[index]) }.skipSummary
        #expect(mix.classes == [.keep, .rescan, .never])
        #expect(CleanWording.advice(mix) == "Quit Google Chrome, then Check Again. Start Docker Desktop, then Check Again. \(footer) "
            + "2 items changed since the scan. Scan Again to see them as they are now. Protected by .ddc-keep or an exclusion.")
    }

    @Test("the after-clean table: every reason has its class, as the story's table lists them")
    func afterCleanTable() throws {
        var classes: [AfterClean: Set<String>] = [:]
        for tag in SkipReason.tags {
            let line = #"{"type":"item","path":"/Users/example/x","rule":"cache.npm","class":"safe","action":"delete","prediction":"would-skip","reason":"\#(tag)","pid":7,"name":"node","program":"docker","detail":"x","lock":{"reason":"recent"}}"#
            guard case .item(let item) = try PreflightLine.read(line), case .wouldSkip(let reason) = item.prediction else {
                Issue.record("\(tag) did not decode as a skip")
                continue
            }
            classes[reason.afterClean, default: []].insert(tag)
        }
        #expect(classes[.keep] == ["owner-running", "in-use", "in-use-unknown-cwd", "tool-not-ready"])
        #expect(classes[.rescan] == [
            "changed-since-scan", "parent-changed", "missing", "rule-mismatch", "not-an-entry", "entry-locked", "symlink",
            "not-a-directory", "incomplete",
        ])
        #expect(classes[.never] == [
            "kept", "protected", "project-root", "scan-root", "outside-scan-roots", "action-not-allowed", "needs-confirmation",
            "inside-another-item", "native-unavailable",
        ])
        #expect(SkipReason.tags.count == 22)
    }
}
