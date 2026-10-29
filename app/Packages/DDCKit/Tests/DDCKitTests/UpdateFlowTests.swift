import Foundation
import Testing
@testable import DDCKit

/// Story "Sparkle updates: check daily, Update and Relaunch, and the cask's
/// auto_updates": the update line's state machine, without Sparkle.
@Suite("Update flow")
struct UpdateFlowTests {
    let item = UpdateFlow.Item(
        version: "13", shortVersion: "0.8.0",
        releaseURL: URL(string: "https://github.com/SmartAndPoint/DeepDriveCleaner/releases/tag/v0.8.0"))

    /// Every reply the flow gives over a sequence of events.
    private func replies(_ effects: [UpdateFlow.Effect]) -> [UpdateFlow.Reply] {
        effects.compactMap { if case .reply(let reply) = $0 { reply } else { nil } }
    }

    @Test func aFoundItemBecomesAvailableAndIsDismissed() {
        var flow = UpdateFlow()
        let effects = flow.handle(.found(item, informationOnly: false))
        #expect(flow.state == .available)
        #expect(flow.item == item)
        #expect(effects == [.record(item, informationOnly: false), .reply(.dismiss)])
    }

    @Test func onePressInstallsOnTheFoundReplyAndOnReadyToInstall() {
        var flow = UpdateFlow()
        #expect(flow.handle(.pressed) == [.checkForUpdates])
        #expect(flow.state == .checking)
        #expect(flow.handle(.checkStarted) == [])
        #expect(replies(flow.handle(.found(item, informationOnly: false))) == [.install])
        #expect(flow.state == .downloading)
        _ = flow.handle(.downloadStarted)
        _ = flow.handle(.extracting)
        #expect(flow.state == .extracting)
        #expect(flow.handle(.readyToInstall) == [.reply(.install)])
        #expect(flow.state == .installing)
        #expect(flow.handle(.postponeAsked(busy: false)) == [])
        #expect(flow.state == .installing)
    }

    @Test func aPressFromAvailableChecksAgainAndInstalls() {
        var flow = UpdateFlow()
        _ = flow.handle(.found(item, informationOnly: false))
        #expect(flow.handle(.pressed) == [.checkForUpdates])
        #expect(replies(flow.handle(.found(item, informationOnly: false))) == [.install])
        #expect(flow.handle(.readyToInstall) == [.reply(.install)])
    }

    @Test func withoutAPressReadyToInstallIsDismissed() {
        var flow = UpdateFlow()
        _ = flow.handle(.found(item, informationOnly: false))
        #expect(flow.handle(.readyToInstall) == [.reply(.dismiss)])
        #expect(flow.state == .available)
    }

    @Test func aPressDuringAnotherStepDoesNothing() {
        var flow = UpdateFlow()
        _ = flow.handle(.pressed)
        #expect(flow.handle(.pressed) == [])
        _ = flow.handle(.found(item, informationOnly: false))
        #expect(flow.handle(.pressed) == [])
        #expect(flow.state == .downloading)
    }

    @Test func aPressWhileTheRunnerHasWorkInstallsNothingUntilItIsIdle() {
        var flow = UpdateFlow()
        _ = flow.handle(.pressed)
        _ = flow.handle(.found(item, informationOnly: false))
        _ = flow.handle(.readyToInstall)
        #expect(flow.handle(.postponeAsked(busy: true)) == [.postpone])
        #expect(flow.state == .waitingForIdle)
        #expect(flow.handle(.installStarted) == [])
        #expect(flow.state == .installing)
    }

    @Test func theHeldRelaunchResumesOnceWhenTheRunnerIsIdle() {
        var flow = UpdateFlow()
        _ = flow.handle(.pressed)
        _ = flow.handle(.found(item, informationOnly: false))
        _ = flow.handle(.readyToInstall)
        _ = flow.handle(.postponeAsked(busy: true))
        #expect(flow.handle(.runnerIdle) == [.resumeInstall])
        #expect(flow.state == .installing)
        #expect(flow.handle(.runnerIdle) == [])
    }

    @Test func aQuitWhileWaitingEndsInInstallingOnQuit() {
        var flow = UpdateFlow()
        _ = flow.handle(.pressed)
        _ = flow.handle(.found(item, informationOnly: false))
        _ = flow.handle(.readyToInstall)
        _ = flow.handle(.postponeAsked(busy: true))
        #expect(flow.handle(.appQuits) == [.dropPostponed])
        #expect(flow.state == .installingOnQuit)
        #expect(flow.handle(.runnerIdle) == [])
        #expect(flow.handle(.appQuits) == [])
        _ = flow.handle(.installStarted)
        #expect(flow.state == .installingOnQuit)
    }

    @Test func aQuitWithNothingWaitingDoesNothing() {
        var flow = UpdateFlow()
        _ = flow.handle(.found(item, informationOnly: false))
        #expect(flow.handle(.appQuits) == [])
        #expect(flow.state == .available)
    }

    @Test func anInformationOnlyItemIsNeverInstalled() {
        var flow = UpdateFlow()
        #expect(replies(flow.handle(.found(item, informationOnly: true))) == [.dismiss])
        #expect(flow.state == .informationOnly)
        #expect(flow.handle(.pressed) == [])
        #expect(flow.handle(.readyToInstall) == [.reply(.dismiss)])
        #expect(flow.state == .informationOnly)
    }

    @Test func aPressThatFindsAnInformationOnlyItemDoesNotInstall() {
        var flow = UpdateFlow()
        _ = flow.handle(.pressed)
        #expect(replies(flow.handle(.found(item, informationOnly: true))) == [.dismiss])
        #expect(flow.pressed == false)
        #expect(flow.handle(.readyToInstall) == [.reply(.dismiss)])
    }

    @Test func anErrorAfterAPressReturnsToAvailableWithTheReleaseLink() {
        var flow = UpdateFlow()
        _ = flow.handle(.found(item, informationOnly: false))
        _ = flow.handle(.pressed)
        _ = flow.handle(.found(item, informationOnly: false))
        _ = flow.handle(.error("The download failed."))
        #expect(flow.state == .available)
        #expect(flow.message == "The download failed.")
        #expect(flow.item?.releaseURL == item.releaseURL)
        #expect(flow.pressed == false)
        _ = flow.handle(.pressed)
        #expect(flow.message == nil)
    }

    @Test func anErrorOfACheckAskedForBeforeAnyItemFails() {
        var flow = UpdateFlow()
        _ = flow.handle(.checkStarted)
        _ = flow.handle(.error("The feed could not be read."))
        #expect(flow.state == .failed)
        #expect(flow.message == "The feed could not be read.")
    }

    @Test func aBackgroundErrorIsNotShown() {
        var flow = UpdateFlow()
        _ = flow.handle(.error("offline"))
        #expect(flow.state == .idle)
        #expect(flow.message == nil)
        _ = flow.handle(.found(item, informationOnly: false))
        _ = flow.handle(.error("offline"))
        #expect(flow.state == .available)
        #expect(flow.message == nil)
    }

    @Test func nothingNewerForgetsTheItem() {
        var flow = UpdateFlow()
        _ = flow.handle(.found(item, informationOnly: false))
        #expect(flow.handle(.notFound) == [.forget])
        #expect(flow.state == .idle)
        #expect(flow.item == nil)
    }

    @Test func aStartThatThrowsGivesUnavailable() {
        var flow = UpdateFlow()
        _ = flow.handle(.startFailed("The updater could not start."))
        #expect(flow.state == .unavailable("The updater could not start."))
        #expect(flow.handle(.pressed) == [])
        #expect(flow.handle(.found(item, informationOnly: false)) == [])
        #expect(flow.state == .unavailable("The updater could not start."))
    }
}
