import Foundation
import Testing
@testable import DDCKit

/// Story "Faster clean: re-check only what the selection needs", decomposition
/// step 1: the app log's line of where a clean's time went.
@Suite("Clean timings")
struct CleanTimingsTests {
    static func walk(_ step: ProgressEvent.WalkStep, _ files: UInt64) throws -> ProgressEvent {
        try ProgressTests.event(ProgressTests.walk(step.rawValue, files, expected: 100))
    }

    @Test("the line names the wait, the re-check, the rest up to the first action with the marker walk bracketed, and the clean")
    func line() throws {
        let t0 = ContinuousClock.now
        let at = { (ms: Int) in t0 + .milliseconds(ms) }
        var timings = CleanTimings(requested: t0)
        timings.started(at: at(200))
        timings.record(.phase(.preparing), at: at(250))
        timings.record(try Self.walk(.recheck, 40_000), at: at(10_000))
        timings.record(try Self.walk(.recheck, 71_250), at: at(28_650))
        // The ticker's first look, before the snapshot count ends: no file yet.
        timings.record(try Self.walk(.markers, 0), at: at(28_700))
        timings.record(try Self.walk(.markers, 300), at: at(28_900))
        timings.record(try Self.walk(.markers, 900), at: at(29_200))
        // The hook's final tick and the phase, in one write after the process snapshot.
        timings.record(try Self.walk(.markers, 1000), at: at(29_500))
        timings.record(.phase(.cleaning), at: at(29_505))
        // Nothing after `cleaning` moves the walk's times.
        timings.record(try Self.walk(.recheck, 99), at: at(29_600))
        timings.ended(at: at(30_300))

        #expect(timings.line == "clean timings: waited 0.2 s · re-check 28.4 s (71,250 files) · then 0.9 s to the first action (marker walk about 0.3 s) · first action 29.5 s after Clean · cleaning 0.8 s")
    }

    @Test("a clean without events, or one that never started, says what it does not know")
    func unknown() {
        let t0 = ContinuousClock.now
        var timings = CleanTimings(requested: t0)
        timings.ended(at: t0 + .seconds(1))
        #expect(timings.line == "clean timings: waited ? · re-check ? · then ? to the first action (marker walk about ?) · first action ? after Clean · cleaning ?")
    }
}
