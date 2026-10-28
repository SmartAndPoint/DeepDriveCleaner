import Foundation
import Testing
@testable import DDCKit

/// Story "CLI: progress events": the decoder, the sheet's reducer and its
/// words. The lines are in the shape `crates/ddc-core/tests/progress.rs` pins.
@Suite("Progress events")
struct ProgressTests {
    static let preparing = #"{"schema_version":2,"event":"phase","phase":"preparing"}"#
    static let cleaning = #"{"schema_version":2,"event":"phase","phase":"cleaning"}"#

    static func walk(_ step: String, _ files: UInt64, expected: UInt64? = nil) -> String {
        let tail = expected.map { #","files_expected":\#($0)"# } ?? ""
        return #"{"schema_version":2,"event":"walk","step":"\#(step)","files":\#(files),"dirs":2,"bytes":4096\#(tail)}"#
    }

    static func items(done: UInt64, n: UInt64, bytesDone: UInt64, bytesTotal: UInt64, current: String? = nil) -> String {
        let tail = current.map { #","current":\#($0)"# } ?? ""
        return #"{"schema_version":2,"event":"items","done":\#(done),"n":\#(n),"bytes_done":\#(bytesDone),"bytes_total":\#(bytesTotal)\#(tail)}"#
    }

    static func current(_ i: UInt64, _ path: String, _ action: String, _ bytes: UInt64, tool: String? = nil) -> String {
        let tail = tool.map { #","tool":"\#($0)""# } ?? ""
        return #"{"i":\#(i),"path":"\#(path)","action":"\#(action)","bytes":\#(bytes)\#(tail)}"#
    }

    static func item(_ i: UInt64, n: UInt64, _ path: String, _ action: String, _ bytes: UInt64, _ outcome: String) -> String {
        #"{"schema_version":2,"event":"item","i":\#(i),"n":\#(n),"path":"\#(path)","action":"\#(action)","bytes":\#(bytes),\#(outcome)}"#
    }

    static func event(_ line: String) throws -> ProgressEvent {
        guard case .event(let event)? = ProgressEvent.read(line) else {
            Issue.record("not an event: \(line)")
            throw CancellationError()
        }
        return event
    }

    static func fold(_ lines: [String]) throws -> CleanProgress {
        var progress = CleanProgress()
        for line in lines { progress.apply(try event(line)) }
        return progress
    }

    // MARK: - The decoder

    @Test("every event decodes; other lines, unknown events and malformed ones are not events; another version says so")
    func decoding() throws {
        #expect(try Self.event(Self.cleaning) == .phase(.cleaning))
        guard case .walk(let walk) = try Self.event(Self.walk("recheck", 3, expected: 5)) else { throw CancellationError() }
        #expect(walk.step == .recheck && walk.files == 3 && walk.dirs == 2 && walk.bytes == 4096 && walk.filesExpected == 5)
        guard case .walk(let scan) = try Self.event(Self.walk("scan", 1)) else { throw CancellationError() }
        #expect(scan.filesExpected == nil)

        let tick = Self.items(done: 0, n: 2, bytesDone: 0, bytesTotal: 10, current: Self.current(1, "/Users/example/.cache/uv", "native", 10, tool: "uv"))
        guard case .items(let items) = try Self.event(tick) else { throw CancellationError() }
        #expect(items.n == 2 && items.bytesTotal == 10)
        #expect(items.current == ProgressEvent.Current(i: 1, path: "/Users/example/.cache/uv", action: .native, bytes: 10, tool: "uv"))

        let skipped = Self.item(2, n: 2, "/Users/example/p/node_modules", "delete", 20, #""outcome":"skipped","reason":"in-use","pid":7,"name":"node""#)
        guard case .item(let item) = try Self.event(skipped) else { throw CancellationError() }
        #expect(item.i == 2 && item.action == .delete && item.bytes == 20)
        #expect(item.outcome == .skipped(.inUse(pid: 7, name: "node")))
        let native = Self.item(1, n: 2, "/Users/example/.cache/uv", "native", 10,
                               #""outcome":"native","commands":[{"program":"uv","args":["cache","clean"],"success":true,"code":0}]"#)
        guard case .item(let ran) = try Self.event(native), case .native(let commands) = ran.outcome else { throw CancellationError() }
        #expect(commands.map(\.program) == ["uv"])

        for line in [
            "ddc: warning: x", "stopping after the current item", "", "{", "{}",
            #"{"schema_version":2,"event":"new-event"}"#,
            #"{"schema_version":2,"event":"phase","phase":"new-phase"}"#,
            #"{"schema_version":2,"event":"item","i":1,"n":1,"path":"/x","action":"delete","bytes":1,"outcome":"new-outcome"}"#,
        ] {
            #expect(ProgressEvent.read(line) == nil, "\(line)")
        }
        #expect(ProgressEvent.read(#"{"schema_version":3,"event":"phase","phase":"cleaning"}"#) == .otherVersion(3))
    }

    // MARK: - The reducer

    @Test("step 1 counts the re-check, then the marker walk; the larger count wins and the estimate is clamped")
    func stepOne() throws {
        var progress = try Self.fold([Self.preparing, Self.walk("recheck", 45_600, expected: 71_300), Self.walk("recheck", 40_000, expected: 71_300)])
        #expect(progress.walk == CleanProgress.Walk(step: .recheck, files: 45_600, expected: 71_300))
        #expect(!progress.cleaning)
        #expect(CleanWording.walkLine(try #require(progress.walk)) == "45,600 of about 71,300 files read")
        #expect(progress.walkFraction == 45_600.0 / 71_300)

        // More files than the estimate: below 1, and the estimate reads as what was read.
        progress.apply(try Self.event(Self.walk("recheck", 80_000, expected: 71_300)))
        #expect(progress.walkFraction == 0.99)
        #expect(CleanWording.walkLine(try #require(progress.walk)) == "80,000 of about 80,000 files read")

        progress.apply(try Self.event(Self.walk("markers", 120, expected: 400)))
        #expect(progress.walk == CleanProgress.Walk(step: .markers, files: 120, expected: 400))
        // A late tick of the earlier step changes nothing.
        progress.apply(try Self.event(Self.walk("recheck", 90_000, expected: 71_300)))
        #expect(progress.walk?.step == .markers)

        // No estimate: a count, no fraction.
        let bare = try Self.fold([Self.walk("recheck", 12, expected: 0)])
        #expect(bare.walkFraction == nil)
        #expect(CleanWording.walkLine(try #require(bare.walk)) == "12 files read")
    }

    @Test("cleaning only moves forward: a walk tick after it is ignored")
    func forwardOnly() throws {
        var progress = try Self.fold([Self.walk("markers", 10, expected: 20), Self.cleaning])
        #expect(progress.cleaning)
        progress.apply(try Self.event(Self.walk("markers", 20, expected: 20)))
        progress.apply(try Self.event(Self.preparing))
        #expect(progress.walk?.files == 10)
        #expect(progress.cleaning)
    }

    @Test("item i of n: an item line counts at once, a repeated one changes nothing, a lagging tick does not go back")
    func items() throws {
        let npm = "/Users/example/.npm/_cacache"
        let chrome = "/Users/example/Library/Caches/Google/Chrome"
        var progress = try Self.fold([
            Self.cleaning,
            Self.items(done: 0, n: 3, bytesDone: 0, bytesTotal: 1000, current: Self.current(1, npm, "delete", 600)),
        ])
        #expect(progress.current?.i == 1)
        #expect(progress.fraction == 0)
        #expect(progress.inFlight == 0.6)

        let deleted = Self.item(1, n: 3, npm, "delete", 600, #""outcome":"deleted""#)
        progress.apply(try Self.event(deleted))
        progress.apply(try Self.event(deleted))
        #expect(progress.done == 1 && progress.bytesDone == 600)
        #expect(progress.current == nil, "the item is done")
        progress.apply(try Self.event(Self.item(2, n: 3, chrome, "delete", 300, #""outcome":"skipped","reason":"owner-running","pid":4242,"name":"Google Chrome""#)))
        #expect(progress.skipped == 1 && progress.done == 2 && progress.bytesDone == 900)

        // A tick written before the item lines: nothing goes back, and a finished item is not in flight.
        progress.apply(try Self.event(Self.items(done: 1, n: 3, bytesDone: 600, bytesTotal: 1000, current: Self.current(2, chrome, "delete", 300))))
        #expect(progress.done == 2 && progress.bytesDone == 900 && progress.current == nil)
        #expect(progress.fraction == 0.9)
        #expect(CleanWording.counterLine(progress) == "900 of 1000 B processed · 2 of 3 items · 1 skipped")
        #expect(CleanWording.percent(progress.fraction) == "90%")

        // A not-run item has no outcome: not done, not skipped.
        progress.apply(try Self.event(Self.item(3, n: 3, "/Users/example/x", "delete", 100, #""outcome":"not-run""#)))
        #expect(progress.done == 2 && progress.skipped == 1)
        #expect(progress.fraction == 0.9)
    }

    @Test("the bar stays below 1 until every item is done, and counts items when no byte is counted")
    func fraction() throws {
        let docker = "/Users/example/Library/Containers/com.docker.docker"
        var progress = try Self.fold([
            Self.cleaning,
            Self.items(done: 1, n: 2, bytesDone: 1000, bytesTotal: 1000, current: Self.current(2, docker, "native-only", 4_000_000, tool: "docker")),
        ])
        #expect(progress.fraction == 0.99)
        #expect(CleanWording.percent(progress.fraction) == "99%")
        progress.apply(try Self.event(Self.items(done: 2, n: 2, bytesDone: 1000, bytesTotal: 1000)))
        #expect(progress.fraction == 1)

        let toolsOnly = try Self.fold([Self.items(done: 1, n: 4, bytesDone: 0, bytesTotal: 0)])
        #expect(toolsOnly.cleaning, "an items tick means the clean began")
        #expect(toolsOnly.fraction == 0.25)
        #expect(CleanWording.counterLine(toolsOnly) == "1 of 4 items processed")
    }

    @Test("a native-only item in flight adds no stripe, and its tool is named with what Stop does")
    func nativeOnlyInFlight() throws {
        let docker = "/Users/example/Library/Containers/com.docker.docker"
        let progress = try Self.fold([
            Self.cleaning,
            Self.items(done: 1, n: 2, bytesDone: 500, bytesTotal: 1000, current: Self.current(2, docker, "native-only", 4_000_000, tool: "docker")),
        ])
        #expect(progress.inFlight == 0)
        let current = try #require(progress.current)
        #expect(CleanWording.now(current, home: "/Users/example") == "Docker")
        #expect(CleanWording.cleaningNote(progress)
            == "Docker reports no progress, and what it frees inside its disk image is not counted here. Stop interrupts Docker; what it already removed stays removed.")

        let path = try Self.fold([
            Self.items(done: 0, n: 2, bytesDone: 0, bytesTotal: 1000, current: Self.current(1, "/Users/example/p1/node_modules", "delete", 400)),
        ])
        #expect(path.inFlight == 0.4)
        #expect(CleanWording.now(try #require(path.current), home: "/Users/example") == "~/p1/node_modules")
        #expect(CleanWording.cleaningNote(path) == "The striped part is the item being cleaned. Stop ends the clean after this item; what is cleaned stays cleaned.")
    }

    @Test("until the CLI's first count the bar reads the sheet's own totals, never 0 of 0")
    func sheetTotalsFirst() throws {
        let early = try Self.fold([Self.cleaning])
        #expect(CleanWording.counterLine(early.counting(items: 6, bytes: 3_366_912)) == "0 B of 3.2 MiB processed · 0 of 6 items")
        let counted = try Self.fold([Self.cleaning, Self.items(done: 0, n: 5, bytesDone: 0, bytesTotal: 2_000_000)])
        #expect(counted.counting(items: 6, bytes: 3_366_912) == counted, "the CLI's n stands once it is known")
    }

    // MARK: - native-only totals (decision 2)

    @Test("frees about and the bar's total leave native-only items out; Selected and the class rows do not")
    @MainActor
    func nativeOnlyTotals() throws {
        let docker = try ReviewFixture.finding(8, [
            "path": "/Users/example/Library/Containers/com.docker.docker", "rule": "vm.docker-desktop",
            "removal": "native-only", "ino": 2, "bytes": 4_294_967_296,
        ])
        let withDocker = try Fixture.editedFindings("scan-report-groups") { $0 += [docker] }
        let items = try ReviewFixture.items(report: withDocker)
        let dockerID = try ReviewFixture.id(items, "com.docker.docker")
        #expect(items.row(dockerID)?.action == .nativeOnly)
        let without = try ReviewFixture.recordedSelection(items)
        let selection = without.union([dockerID])

        let plain = DDCKit.Confirmation(items, selection: without, deletePermanently: false)
        let confirmation = DDCKit.Confirmation(items, selection: selection, deletePermanently: false)
        #expect(confirmation.bytes == plain.bytes + 4_294_967_296)
        #expect(confirmation.reclaimable == plain.reclaimable)
        #expect(confirmation.counted == plain.bytes)
        #expect(plain.counted == plain.bytes)
        #expect(confirmation.classRows.first { $0.riskClass == .review }?.bytes == 700_416 + 4_294_967_296)
        #expect(CleanWording.notCounted(confirmation) == "4.0 GiB inside Docker's disk image is pruned there and not counted.")
        #expect(CleanWording.notCounted(plain) == nil)

        // The Review window's footer names the sheet's numbers.
        let data = try TempDataDirectory()
        let defaults = try TempDefaults()
        let snapshot = try ReviewFixture.snapshot(report: withDocker, settingsIn: data)
        let review = makeReview(runner: nil, data: data, defaults: defaults) { snapshot }
        _ = review.open(ReviewRequest(group: nil, classes: [.safe, .rebuildable], confirm: false))
        review.selection = selection
        #expect(review.selectedBytes == confirmation.bytes)
        #expect(review.selectedReclaimable == confirmation.reclaimable)
    }
}
