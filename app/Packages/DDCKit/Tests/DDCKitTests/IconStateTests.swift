import Testing
@testable import DDCKit

@Suite("Status item icon")
struct IconStateTests {
    static let threshold: UInt64 = 10 << 30

    struct Case: Sendable, CustomTestStringConvertible {
        let name: String
        var busy = false
        var paused = false
        var attention: [IconState.Attention] = []
        var reclaimable: UInt64? = nil
        var showSize = true
        var badgePending = false
        var growthPending = false
        var showBadge = true
        let inner: IconState.Inner
        let mark: IconState.Mark
        let label: String?

        var testDescription: String { name }
    }

    static let cases: [Case] = [
        Case(name: "idle, no report", inner: .dot, mark: .none, label: nil),
        Case(name: "idle, under the threshold", reclaimable: threshold - 1, inner: .dot, mark: .none, label: nil),
        Case(name: "at the threshold: the size, no mark", reclaimable: threshold, inner: .dot, mark: .none, label: "10.0 GiB"),
        Case(name: "over the threshold, size hidden: no mark", reclaimable: threshold + 1, showSize: false, inner: .dot, mark: .none, label: nil),
        Case(name: "nothing to free never counts as over", reclaimable: 0, inner: .dot, mark: .none, label: nil),
        Case(name: "busy", busy: true, inner: .arc, mark: .none, label: nil),
        Case(name: "busy over the threshold keeps the last size", busy: true, reclaimable: threshold, inner: .arc, mark: .none, label: "10.0 GiB"),
        Case(name: "needs Full Disk Access", attention: [.fullDiskAccess], inner: .none, mark: .warning, label: nil),
        Case(name: "a failed scan", attention: [.scanFailed], inner: .none, mark: .warning, label: nil),
        Case(name: "an unreadable report", attention: [.unreadableReport], inner: .none, mark: .warning, label: nil),
        Case(name: "unreadable settings", attention: [.unreadableSettings], inner: .none, mark: .warning, label: nil),
        Case(name: "a missing CLI", attention: [.missingCLI], inner: .none, mark: .warning, label: nil),
        Case(name: "the warning wins over the badge", attention: [.scanFailed], reclaimable: threshold, badgePending: true, inner: .none, mark: .warning, label: "10.0 GiB"),
        Case(name: "the badge", badgePending: true, inner: .none, mark: .badge, label: nil),
        Case(name: "the badge preference off", reclaimable: threshold, badgePending: true, showBadge: false, inner: .dot, mark: .none, label: "10.0 GiB"),
        Case(name: "used space grew: the same badge", growthPending: true, inner: .none, mark: .badge, label: nil),
        Case(name: "both reasons: one badge", badgePending: true, growthPending: true, inner: .none, mark: .badge, label: nil),
        Case(name: "the warning wins over the growth badge", attention: [.fullDiskAccess], growthPending: true, inner: .none, mark: .warning, label: nil),
        Case(name: "the badge preference off hides the growth badge too", growthPending: true, showBadge: false, inner: .dot, mark: .none, label: nil),
        Case(name: "paused", paused: true, inner: .dot, mark: .none, label: nil),
        Case(name: "paused keeps its warning", paused: true, attention: [.fullDiskAccess], inner: .none, mark: .warning, label: nil),
        Case(name: "paused while a scan finishes: a grey arc", busy: true, paused: true, inner: .arc, mark: .none, label: nil),
    ]

    @Test("the icon state covers every combination", arguments: cases)
    func iconState(_ c: Case) {
        let state = IconState.make(
            busy: c.busy,
            paused: c.paused,
            attention: c.attention,
            reclaimable: c.reclaimable,
            threshold: Self.threshold,
            showSize: c.showSize,
            badgePending: c.badgePending,
            growthPending: c.growthPending,
            showBadge: c.showBadge
        )
        #expect(state.inner == c.inner)
        #expect(state.mark == c.mark)
        #expect(state.dimmed == c.paused)
        #expect(state.label == c.label)
        #expect(state.accessibility.hasPrefix("DeepDriveCleaner"))
    }

    @Test("the accessibility description names what the marks show")
    func accessibility() {
        let state = IconState.make(
            busy: true, paused: true, attention: [.scanFailed, .fullDiskAccess], reclaimable: Self.threshold,
            threshold: Self.threshold, showSize: false, badgePending: false, showBadge: true
        )
        #expect(state.accessibility == "DeepDriveCleaner, paused, scanning, needs Full Disk Access, 10.0 GiB can be freed")
    }

    @Test("VoiceOver names the badge's reason: new results, used space grew, or both; nothing when the badge is off")
    func badgeWords() {
        func words(badge: Bool, growth: Bool, show: Bool = true) -> String {
            IconState.make(
                busy: false, reclaimable: nil, threshold: Self.threshold, showSize: false,
                badgePending: badge, growthPending: growth, showBadge: show
            ).accessibility
        }
        #expect(words(badge: true, growth: false) == "DeepDriveCleaner, new results to review")
        #expect(words(badge: false, growth: true) == "DeepDriveCleaner, used space grew")
        #expect(words(badge: true, growth: true) == "DeepDriveCleaner, new results to review, used space grew")
        #expect(words(badge: true, growth: true, show: false) == "DeepDriveCleaner")
    }

    @Test("a clean in flight turns the arc and is named as a clean")
    func cleaning() {
        let state = IconState.make(
            busy: false, cleaning: true, reclaimable: 0, threshold: Self.threshold, showSize: false,
            badgePending: false, showBadge: true
        )
        #expect(state.inner == .arc)
        #expect(state.accessibility == "DeepDriveCleaner, cleaning")
    }
}
