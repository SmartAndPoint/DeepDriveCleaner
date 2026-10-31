import Foundation
import Testing
@testable import DDCKit

/// Story "Sparkle updates: check daily, Update and Relaunch, and the cask's
/// auto_updates": the postpone handler and the kept update.
@Suite("Update model")
@MainActor
final class UpdateModelTests {
    let defaults: TempDefaults
    let preferences: AppPreferences
    let feed = URL(string: "https://github.com/SmartAndPoint/DeepDriveCleaner/releases/latest/download/appcast.xml")!
    let debugFeed = URL(string: "http://127.0.0.1:8089/appcast.xml")!
    let item = UpdateFlow.Item(
        version: "13", shortVersion: "0.8.0",
        releaseURL: URL(string: "https://github.com/SmartAndPoint/DeepDriveCleaner/releases/tag/v0.8.0"))
    var busy = false
    var logged: [String] = []

    init() throws {
        defaults = try TempDefaults()
        preferences = AppPreferences(defaults: defaults.defaults)
    }

    private func model(feed: URL? = nil, bundleVersion: String = "12") -> UpdateModel {
        UpdateModel(
            preferences: preferences,
            feedURL: feed ?? self.feed,
            bundleVersion: bundleVersion,
            busy: { [unowned self] in self.busy },
            log: { [unowned self] in self.logged.append($0) }
        )
    }

    /// A model whose pressed install reached Sparkle's postpone question.
    private func readyToRelaunch() -> UpdateModel {
        let model = model()
        _ = model.send(.found(item, informationOnly: false))
        model.press()
        _ = model.send(.found(item, informationOnly: false))
        _ = model.send(.readyToInstall)
        return model
    }

    // MARK: Progress

    @Test func progressIsKeptOnlyWhileTheDownloadOrTheExtractionRuns() {
        let model = model()
        model.setProgress(0.5)
        #expect(model.progress == nil)
        _ = model.send(.found(item, informationOnly: false))
        model.press()
        _ = model.send(.found(item, informationOnly: false))
        #expect(model.state == .downloading)
        model.setProgress(0.25)
        #expect(model.progress == 0.25)
        model.setProgress(1.5)
        #expect(model.progress == 1)
        _ = model.send(.extracting)
        #expect(model.progress == nil)
        model.setProgress(0.75)
        #expect(model.progress == 0.75)
        _ = model.send(.readyToInstall)
        #expect(model.state == .installing)
        #expect(model.progress == nil)
        model.setProgress(0.5)
        #expect(model.progress == nil)
    }

    // MARK: Postponing

    @Test func withTheRunnerBusyTheRelaunchIsHeldAndResumesOnce() {
        let model = readyToRelaunch()
        busy = true
        var resumed = 0
        #expect(model.shouldPostpone { resumed += 1 })
        #expect(model.state == .waitingForIdle)
        #expect(model.jobsHeld)
        busy = false
        model.runnerIdle()
        #expect(resumed == 1)
        #expect(model.state == .installing)
        model.runnerIdle()
        #expect(resumed == 1)
    }

    @Test func withTheRunnerIdleTheRelaunchIsNotHeld() {
        let model = readyToRelaunch()
        var resumed = 0
        #expect(model.shouldPostpone { resumed += 1 } == false)
        model.runnerIdle()
        #expect(resumed == 0)
        #expect(model.state == .installing)
    }

    @Test func aQuitWhileWaitingDropsTheHandler() {
        let model = readyToRelaunch()
        busy = true
        var resumed = 0
        #expect(model.shouldPostpone { resumed += 1 })
        model.appQuits()
        #expect(model.state == .installingOnQuit)
        model.runnerIdle()
        model.appQuits()
        #expect(resumed == 0)
    }

    @Test func aPostponeQuestionWithoutAPressStillHoldsTheRelaunch() {
        // An update a past session downloaded: Sparkle resumes it without a press.
        let model = model()
        var held: [Bool] = []
        model.onJobsHeld = { held.append($0) }
        var resumed = 0
        busy = true
        _ = model.send(.found(item, informationOnly: false))
        #expect(model.shouldPostpone { resumed += 1 })
        #expect(model.state == .waitingForIdle)
        #expect(held == [true])
        model.runnerIdle()
        #expect(resumed == 1)
    }

    @Test func aPostponeQuestionWithTheRunnerIdleHoldsTheJobsForTheInstall() {
        let model = model()
        var held: [Bool] = []
        model.onJobsHeld = { held.append($0) }
        _ = model.send(.found(item, informationOnly: false))
        model.press()
        _ = model.send(.found(item, informationOnly: false))
        #expect(model.shouldPostpone {} == false)
        #expect(model.state == .installing)
        #expect(held == [true])
    }

    @Test func aPostponeQuestionForAnInformationOnlyUpdateHoldsNothing() {
        let model = model()
        busy = true
        _ = model.send(.found(item, informationOnly: true))
        #expect(model.shouldPostpone {} == false)
        #expect(model.state == .informationOnly)
    }

    @Test func aFailedHeldInstallNeverResumes() {
        let model = model()
        var resumed = 0
        busy = true
        _ = model.send(.found(item, informationOnly: false))
        #expect(model.shouldPostpone { resumed += 1 })
        _ = model.send(.error("The install failed."))
        busy = false
        model.runnerIdle()
        #expect(resumed == 0)
        #expect(model.jobsHeld == false)
        #expect(logged.contains("update: the held install failed, it does not resume"))
    }

    @Test func anEndedSessionReleasesTheJobs() {
        let model = readyToRelaunch()
        var held: [Bool] = []
        model.onJobsHeld = { held.append($0) }
        #expect(model.jobsHeld)
        _ = model.send(.sessionEnded)
        #expect(model.state == .available)
        #expect(held == [false])
        #expect(model.canPress)
    }

    @Test func theVersionRowSaysWhatTheLastCheckFound() {
        let model = model()
        let checked = Date(timeIntervalSince1970: 1_800_000_000)
        #expect(model.versionStatus(lastCheck: nil) == "Not checked yet")
        _ = model.send(.checkStarted)
        #expect(model.versionStatus(lastCheck: nil) == "Checking…")
        _ = model.send(.notFound)
        #expect(model.versionStatus(lastCheck: checked) == "Up to date")
        _ = model.send(.found(item, informationOnly: false))
        #expect(model.versionStatus(lastCheck: checked) == "0.8.0 is available")
    }

    @Test func theVersionRowNamesAKeptUpdateAndAFailedCheck() {
        preferences.setFoundUpdate(UpdateRecord(
            version: "13", shortVersion: "0.8.0", releaseURL: nil, feedURL: feed, informationOnly: false))
        #expect(model().versionStatus(lastCheck: nil) == "0.8.0 is available")
        preferences.setFoundUpdate(nil)
        let fresh = model()
        _ = fresh.send(.checkStarted)
        _ = fresh.send(.error("offline"))
        #expect(fresh.versionStatus(lastCheck: Date()) == "The last check failed")
        _ = fresh.send(.startFailed("no feed"))
        #expect(fresh.versionStatus(lastCheck: Date()) == nil)
    }

    @Test func checkNowWaitsForTheLineToRest() {
        let model = model()
        #expect(model.atRest)
        _ = model.send(.checkStarted)
        #expect(model.atRest == false)
        _ = model.send(.found(item, informationOnly: false))
        #expect(model.atRest)
        model.press()
        #expect(model.atRest == false)
        _ = model.send(.found(item, informationOnly: false))
        #expect(model.atRest == false)
        _ = model.send(.sessionEnded)
        #expect(model.atRest)
    }

    @Test func theJobsAreHeldWhileTheRelaunchWaitsOrRuns() {
        let model = model()
        var held: [Bool] = []
        model.onJobsHeld = { held.append($0) }
        _ = model.send(.found(item, informationOnly: false))
        model.press()
        _ = model.send(.found(item, informationOnly: false))
        #expect(held == [])
        _ = model.send(.readyToInstall)
        #expect(held == [true])
        busy = true
        _ = model.shouldPostpone {}
        model.runnerIdle()
        #expect(held == [true])
        _ = model.send(.error("The install failed."))
        #expect(held == [true, false])
        #expect(model.jobsHeld == false)
    }

    // MARK: Update and Relaunch

    @Test func aPressStartsACheckOnlyWhenTheRunnerIsIdle() {
        let model = model()
        var checks = 0
        model.startCheck = { checks += 1 }
        model.press()
        #expect(checks == 0, "nothing to install yet")
        _ = model.send(.found(item, informationOnly: false))
        busy = true
        #expect(model.canPress == false)
        model.press()
        #expect(checks == 0)
        busy = false
        #expect(model.canPress)
        model.press()
        #expect(checks == 1)
        #expect(model.state == .checking)
        #expect(model.canPress == false)
    }

    @Test func anInformationOnlyUpdateCannotBePressed() {
        let model = model()
        _ = model.send(.found(item, informationOnly: true))
        #expect(model.informationOnly)
        #expect(model.canPress == false)
    }

    // MARK: The kept update

    @Test func aFoundUpdateIsKeptAcrossLaunches() {
        _ = model().send(.found(item, informationOnly: false))
        let next = model()
        #expect(next.item == item)
        #expect(next.canPress)
        #expect(next.state == .idle)
    }

    @Test func theKeptUpdateIsClearedWhenTheRunningBuildReachesIt() {
        _ = model().send(.found(item, informationOnly: false))
        #expect(model(bundleVersion: "13").item == nil)
        #expect(preferences.foundUpdate == nil)
        _ = model().send(.found(item, informationOnly: false))
        #expect(model(bundleVersion: "14").item == nil)
        #expect(preferences.foundUpdate == nil)
    }

    @Test func theKeptUpdateIsComparedAsANumber() {
        let later = UpdateFlow.Item(version: "100", shortVersion: "1.0.0", releaseURL: nil)
        _ = model().send(.found(later, informationOnly: false))
        #expect(model(bundleVersion: "99").item == later)
        #expect(preferences.foundUpdate != nil)
    }

    @Test func anotherFeedsRecordIsIgnoredAndLeftInPlace() {
        _ = model(feed: debugFeed).send(.found(item, informationOnly: false))
        let release = model()
        #expect(release.item == nil)
        #expect(release.canPress == false)
        #expect(preferences.foundUpdate?.feedURL == debugFeed)
        _ = release.send(.notFound)
        #expect(preferences.foundUpdate?.feedURL == debugFeed)
    }

    @Test func nothingNewerForgetsTheKeptUpdate() {
        _ = model().send(.found(item, informationOnly: false))
        let next = model()
        _ = next.send(.notFound)
        #expect(next.item == nil)
        #expect(preferences.foundUpdate == nil)
    }

    @Test func anInformationOnlyUpdateIsKeptAsSuch() {
        _ = model().send(.found(item, informationOnly: true))
        let next = model()
        #expect(next.informationOnly)
        #expect(next.canPress == false)
    }
}
