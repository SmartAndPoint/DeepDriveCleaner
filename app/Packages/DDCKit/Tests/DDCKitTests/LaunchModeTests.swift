import Testing
@testable import DDCKit

@Suite("LaunchMode")
struct LaunchModeTests {
    @Test("a --scheduled start has no status item and quits when its work is done")
    func scheduledQuits() {
        var mode = LaunchMode()
        #expect(mode.handle(.launched(arguments: ["/app", "--scheduled"])) == [.startScheduledWork])
        #expect(mode.mode == .scheduled)
        #expect(mode.handle(.workFinished) == [.quit])
    }

    @Test("a user's event during scheduled work promotes the instance, which then keeps running",
          arguments: [LaunchMode.Event.reopen(windowOpen: false), .activate, .notificationResponse])
    func promotion(event: LaunchMode.Event) {
        var mode = LaunchMode()
        _ = mode.handle(.launched(arguments: ["/app", "--scheduled"]))

        #expect(mode.handle(event) == [.showStatusItem, .startUpdater])
        #expect(mode.mode == .normal)
        #expect(mode.handle(.workFinished) == [])
    }

    @Test("a normal start shows the status item and never quits on its own")
    func normalNeverQuits() {
        var mode = LaunchMode()
        #expect(mode.handle(.launched(arguments: ["/app"])) == [.showStatusItem, .startUpdater])
        for event in [LaunchMode.Event.workFinished, .activate, .notificationResponse, .workFinished] {
            #expect(!mode.handle(event).contains(.quit))
        }
        #expect(mode.mode == .normal)
    }

    @Test("a reopen of the full app, the scheduler's tick among them, is a scan if due")
    func reopenOfNormalApp() {
        var mode = LaunchMode()
        _ = mode.handle(.launched(arguments: ["/app"]))
        #expect(mode.handle(.reopen(windowOpen: false)) == [.scanIfDue])
    }

    @Test("a click on the Dock icon while a window is open also brings the windows to the front")
    func reopenWithAWindowOpen() {
        var mode = LaunchMode()
        _ = mode.handle(.launched(arguments: ["/app"]))
        #expect(mode.handle(.reopen(windowOpen: true)) == [.scanIfDue, .frontWindows])
    }

    // Story "Sparkle updates: check daily, Update and Relaunch, and the cask's auto_updates".

    /// Every effect a sequence of events produces.
    private func effects(_ events: [LaunchMode.Event]) -> [LaunchMode.Effect] {
        var mode = LaunchMode()
        return events.flatMap { mode.handle($0) }
    }

    @Test("a --scheduled launch that finishes its work never starts the updater")
    func scheduledNeverStartsTheUpdater() {
        #expect(!effects([.launched(arguments: ["/app", "--scheduled"]), .workFinished]).contains(.startUpdater))
    }

    @Test("a --scheduled launch promoted by a user's event starts the updater once",
          arguments: [LaunchMode.Event.reopen(windowOpen: false), .activate, .notificationResponse])
    func promotedStartsTheUpdaterOnce(event: LaunchMode.Event) {
        let all = effects([
            .launched(arguments: ["/app", "--scheduled"]), event, .workFinished,
            .reopen(windowOpen: false), .activate, .notificationResponse, .reopen(windowOpen: true),
        ])
        #expect(all.filter { $0 == .startUpdater }.count == 1)
        #expect(all.firstIndex(of: .showStatusItem)! < all.firstIndex(of: .startUpdater)!)
    }

    @Test("a normal launch starts the updater once, after the status item")
    func normalStartsTheUpdaterOnce() {
        let all = effects([
            .launched(arguments: ["/app"]), .reopen(windowOpen: false), .activate,
            .notificationResponse, .workFinished, .reopen(windowOpen: true),
        ])
        #expect(all.filter { $0 == .startUpdater }.count == 1)
        #expect(Array(all.prefix(2)) == [.showStatusItem, .startUpdater])
    }
}
