import Testing
@testable import DDCKit

@Suite("Status item icon")
struct IconStateTests {
    static let threshold: UInt64 = 10 << 30

    struct Case: Sendable, CustomTestStringConvertible {
        let name: String
        var busy = false
        var reclaimable: UInt64? = nil
        var showSize = true
        var access = true
        var badgePending = false
        var showBadge = true
        let inner: IconState.Inner
        let mark: IconState.Mark
        let label: String?

        var testDescription: String { name }
    }

    static let cases: [Case] = [
        Case(name: "idle, no report", inner: .dot, mark: .none, label: nil),
        Case(name: "idle, under the threshold", reclaimable: threshold - 1, inner: .dot, mark: .none, label: nil),
        Case(name: "at the threshold, with the size", reclaimable: threshold, inner: .none, mark: .dot, label: "10.0 GiB"),
        Case(name: "over the threshold, size hidden", reclaimable: threshold + 1, showSize: false, inner: .none, mark: .dot, label: nil),
        Case(name: "nothing to free never counts as over", reclaimable: 0, inner: .dot, mark: .none, label: nil),
        Case(name: "busy", busy: true, inner: .arc, mark: .none, label: nil),
        Case(name: "busy over the threshold keeps the last size", busy: true, reclaimable: threshold, inner: .arc, mark: .dot, label: "10.0 GiB"),
        Case(name: "needs Full Disk Access", access: false, inner: .none, mark: .alert, label: nil),
        Case(name: "Full Disk Access wins over the badge and the dot", reclaimable: threshold, access: false, badgePending: true, inner: .none, mark: .alert, label: "10.0 GiB"),
        Case(name: "the badge wins over the dot", reclaimable: threshold, badgePending: true, inner: .none, mark: .badge, label: "10.0 GiB"),
        Case(name: "the badge preference off", reclaimable: threshold, badgePending: true, showBadge: false, inner: .none, mark: .dot, label: "10.0 GiB"),
        Case(name: "the badge preference off, under the threshold", badgePending: true, showBadge: false, inner: .dot, mark: .none, label: nil),
    ]

    @Test("the icon state covers every combination", arguments: cases)
    func iconState(_ c: Case) {
        let state = IconState.make(
            busy: c.busy,
            reclaimable: c.reclaimable,
            threshold: Self.threshold,
            showSize: c.showSize,
            fullDiskAccess: c.access,
            badgePending: c.badgePending,
            showBadge: c.showBadge
        )
        #expect(state.inner == c.inner)
        #expect(state.mark == c.mark)
        #expect(state.label == c.label)
        #expect(state.accessibility.hasPrefix("DeepDriveCleaner"))
    }

    @Test("the accessibility description names what the marks show")
    func accessibility() {
        let state = IconState.make(
            busy: true, reclaimable: Self.threshold, threshold: Self.threshold, showSize: false,
            fullDiskAccess: false, badgePending: false, showBadge: true
        )
        #expect(state.accessibility == "DeepDriveCleaner, scanning, needs Full Disk Access, 10.0 GiB can be freed")
    }
}
