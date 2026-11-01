import Foundation
import Testing
@testable import DDCKit

/// Story "Confirmation sheet: Delete permanently for every item at once".
/// Temporary directories and defaults suites only.
@Suite("Confirmation sheet: Delete all permanently")
@MainActor
struct DeleteAllPermanentlyTests {
    let data: TempDataDirectory
    let defaults: TempDefaults

    init() throws {
        data = try TempDataDirectory()
        defaults = try TempDefaults()
    }

    /// The groups fixture's review item 8, two more review items the Trash
    /// takes, and a tool's item.
    private func report() throws -> Data {
        let second = try ReviewFixture.finding(8, ["path": "/Users/example/.cache/huggingface", "rule": "model.huggingface", "ino": 1])
        let third = try ReviewFixture.finding(8, ["path": "/Users/example/.cache/torch", "rule": "model.huggingface", "ino": 3])
        let podman = try ReviewFixture.finding(8, ["path": "/Users/example/.local/share/containers", "rule": "vm.podman", "removal": "native-only", "ino": 2])
        return try Fixture.editedFindings("scan-report-groups") { $0 += [second, third, podman] }
    }

    @Test("the sheet-wide switch sets every item that can be permanent, and only those; its state follows the items")
    func confirmationState() throws {
        let items = try ReviewFixture.items(report: try report())
        let safe = try #require(items.rows.first { $0.riskClass == .safe && $0.selectable }).id
        var sheet = DDCKit.Confirmation(items, selection: [8, 11, 12, 13, safe], deletePermanently: false)
        let switchable = sheet.items.filter(\.canBePermanent).map(\.id)
        #expect(switchable.count == 3)
        let others = sheet.items.filter { !$0.canBePermanent }
        #expect(others.count == 2)
        #expect(sheet.permanentState == .off && sheet.showsAllPermanent)

        do {
            let changed = sheet.setAllPermanent(true)
            #expect(changed)
        }
        #expect(sheet.permanentState == .on)
        #expect(sheet.items.filter(\.permanent).map(\.id) == switchable)
        #expect(sheet.items.filter { !$0.canBePermanent } == others, "the other items are untouched")
        #expect(sheet.items.filter(\.canBePermanent).allSatisfy { $0.effectiveAction == .delete })

        sheet.togglePermanent(switchable[0])
        #expect(sheet.permanentState == .mixed)
        do {
            let changed = sheet.setAllPermanent(true)
            #expect(changed)
        }
        #expect(sheet.permanentState == .on)
        do {
            let changed = sheet.setAllPermanent(false)
            #expect(changed)
        }
        #expect(sheet.permanentState == .off)
        do {
            let changed = sheet.setAllPermanent(false)
            #expect(!changed, "nothing changed")
        }
    }

    @Test("the switch shows from two items that can be permanent; with none it changes nothing")
    func threshold() throws {
        let items = try ReviewFixture.items(report: try report())
        var none = DDCKit.Confirmation(items, selection: [13], deletePermanently: false)
        #expect(none.permanentState == .off && !none.showsAllPermanent)
        do {
            let changed = none.setAllPermanent(true)
            #expect(!changed)
        }
        #expect(!DDCKit.Confirmation(items, selection: [8], deletePermanently: false).showsAllPermanent)
        #expect(DDCKit.Confirmation(items, selection: [8, 11], deletePermanently: false).showsAllPermanent)
    }

    @Test("the clean list after the switch is the one ticking each item gives: delete, per item")
    func listedPerItem() throws {
        let items = try ReviewFixture.items(report: try report())
        var all = DDCKit.Confirmation(items, selection: [8, 11, 12, 13], deletePermanently: false)
        all.setAllPermanent(true)
        var byHand = DDCKit.Confirmation(items, selection: [8, 11, 12, 13], deletePermanently: false)
        for item in byHand.items where item.canBePermanent { byHand.togglePermanent(item.id) }
        #expect(try all.itemList(checkedAgainst: items).encoded() == byHand.itemList(checkedAgainst: items).encoded())
        let json = try JSONSerialization.jsonObject(with: all.itemList(checkedAgainst: items).encoded()) as? [String: Any]
        let listed = try #require(json?["items"] as? [[String: Any]])
        #expect(listed.map { $0["action"] as? String } == ["delete", "delete", "delete", "native-only"])
    }

    @Test("in the sheet the switch runs the check again when it changed something, and never touches the preference")
    func reviewModel() throws {
        let snapshot = try ReviewFixture.snapshot(report: try report(), settingsIn: data)
        let review = makeReview(runner: nil, data: data, defaults: defaults) { snapshot }
        _ = review.open(ReviewRequest(group: nil, classes: [.safe, .rebuildable, .review], confirm: false))
        review.selection = [8, 11, 12]
        review.openSheet()
        review.sheet?.warnings = .checked

        review.setAllPermanent(true)
        #expect(review.sheet?.confirmation.permanentState == .on)
        // With no embedded CLI a check fails at once: that it failed shows it ran.
        #expect(review.sheet?.warnings == .failed("The app's embedded CLI is missing. Reinstall DeepDriveCleaner."))

        review.sheet?.warnings = .checked
        review.setAllPermanent(true)
        #expect(review.sheet?.warnings == .checked, "nothing changed, no check")
        #expect(!defaults.defaults.bool(forKey: AppPreferences.deletePermanentlyKey))

        review.sheet?.stage = .cleaning(.waiting)
        review.setAllPermanent(false)
        #expect(review.sheet?.confirmation.permanentState == .on, "only at the confirm stage")
    }
}
