import Testing
@testable import DDCKit

@Suite("DockPresence")
struct DockPresenceTests {
    @Test("the first titled window makes the app regular; more windows change nothing")
    func firstWindow() {
        var presence = DockPresence<String>()
        #expect(!presence.isRegular)
        #expect(presence.willShow("review") == [.regular])
        #expect(presence.isRegular)
        #expect(presence.willShow("settings") == [])
        #expect(presence.willShow("review") == [], "showing an open window again")
    }

    @Test("the last close returns to the menu bar and gives the focus back")
    func lastClose() {
        var presence = DockPresence<String>()
        _ = presence.willShow("review")
        _ = presence.willShow("about")
        #expect(presence.didClose("about") == [], "the Review window is still open")
        #expect(presence.isRegular)
        #expect(presence.didClose("review") == [.accessory, .yieldActivation])
        #expect(!presence.isRegular)
    }

    @Test("a close that leads to the popover keeps the focus")
    func closeToPopover() {
        var presence = DockPresence<String>()
        _ = presence.willShow("review")
        #expect(presence.didClose("review", returningToPopover: true) == [.accessory])
    }

    @Test("closing a window never shown, or twice, changes nothing")
    func unknownClose() {
        var presence = DockPresence<String>()
        #expect(presence.didClose("popover") == [])
        _ = presence.willShow("settings")
        _ = presence.didClose("settings")
        #expect(presence.didClose("settings") == [])
        #expect(presence.willShow("settings") == [.regular], "opened again")
    }
}
