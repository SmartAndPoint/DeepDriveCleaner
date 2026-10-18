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
          arguments: [LaunchMode.Event.reopen, .activate, .notificationResponse])
    func promotion(event: LaunchMode.Event) {
        var mode = LaunchMode()
        _ = mode.handle(.launched(arguments: ["/app", "--scheduled"]))

        #expect(mode.handle(event) == [.showStatusItem])
        #expect(mode.mode == .normal)
        #expect(mode.handle(.workFinished) == [])
    }

    @Test("a normal start shows the status item and never quits on its own")
    func normalNeverQuits() {
        var mode = LaunchMode()
        #expect(mode.handle(.launched(arguments: ["/app"])) == [.showStatusItem])
        for event in [LaunchMode.Event.workFinished, .activate, .notificationResponse, .workFinished] {
            #expect(!mode.handle(event).contains(.quit))
        }
        #expect(mode.mode == .normal)
    }

    @Test("a reopen of the full app, the scheduler's tick among them, is a scan if due")
    func reopenOfNormalApp() {
        var mode = LaunchMode()
        _ = mode.handle(.launched(arguments: ["/app"]))
        #expect(mode.handle(.reopen) == [.scanIfDue])
    }
}
