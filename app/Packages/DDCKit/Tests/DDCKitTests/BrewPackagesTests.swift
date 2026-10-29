import Foundation
import Testing
@testable import DDCKit

/// The Homebrew packages sheet's model (story "Whole disk: Homebrew
/// packages, their sizes, and brew uninstall"), over a CLI that answers from
/// memory. Nothing here runs brew.
@MainActor
@Suite("Whole disk: Homebrew packages and brew uninstall")
struct BrewPackagesTests {
    private func listing(_ edit: (inout [String: Any]) -> Void = { _ in }) throws -> Data {
        try Fixture.edited("brew-packages", edit)
    }

    private func runner(_ data: Data) async -> FakeRunner {
        let runner = FakeRunner(scan: FakeRunner.done(Data()))
        await runner.reply(.brewPackages, FakeRunner.done(data))
        return runner
    }

    private func report(_ action: [String: Any]) throws -> FreeReport.ActionReport {
        try FreeReport.read(Fixture.edited("free-report") { json in
            var actions = json["actions"] as! [[String: Any]]
            for (key, value) in action { actions[0][key] = value }
            json["actions"] = actions
        }).actions[0]
    }

    @Test("the listing decodes strictly, with every field the sheet shows")
    func decodes() throws {
        let read = try BrewPackages.read(Fixture.data("brew-packages"))
        #expect(read.cellar == "/opt/homebrew/Cellar" && read.services == .listed && read.incomplete.isEmpty)
        #expect(read.formulae.map(\.fullName) == ["node", "wget", "openssl@3", "postgresql@16", "example/tap/foo"])
        let node = read.formulae[0]
        #expect(node.kegs.map(\.version) == ["22.1.0", "23.0.0"] && node.bytes == 213_909_504)
        let foo = read.formulae[4]
        #expect(foo.name == "foo" && foo.bytes == nil && foo.kegs[0].bytes == nil && foo.keptPinned == ["bar"])
        #expect(throws: ScanReport.ReadError.otherSchema(2)) {
            try BrewPackages.read(try listing { $0["schema_version"] = 2 })
        }
        #expect(throws: DecodingError.self) {
            try BrewPackages.read(try listing { json in
                var formulae = json["formulae"] as! [[String: Any]]
                formulae[0]["service"] = "maybe"
                json["formulae"] = formulae
            })
        }
    }

    @Test("Uninstall… on what brew lets go, with its kegs; each other row says why not")
    func offers() async throws {
        let model = BrewPackagesModel(runner: await runner(try Fixture.data("brew-packages")), log: { _ in })
        await model.open()
        let read = try #require(model.listing)
        #expect(model.isOpen && model.message == nil && !model.asking)
        let offers = read.formulae.map { model.offer($0) }
        #expect(offers.map { $0?.fullListed } == [
            #"{"action":"brew-uninstall","identifier":"node","kegs":["22.1.0","23.0.0"]}"#,
            #"{"action":"brew-uninstall","identifier":"wget","kegs":["1.24.5"]}"#,
            nil, nil, nil,
        ])
        #expect(offers[0]?.buttonTitle == "Uninstall…" && offers[0]?.question == "Uninstall node?")
        #expect(offers[0]?.previewingText == "Checking with Homebrew…")
        #expect(offers[0]?.consequence.contains("such as a virtualenv linked against Homebrew's openssl") == true)
        #expect(read.formulae.map(BrewWording.notOffered) == [
            nil, nil,
            "Needed by node, wget and the cask example-app.",
            "A service of yours is loaded for it; stop it with brew services first.",
            "Pinned: brew unpin it first.",
        ])

        // brew listed a formula without a field: nothing is offered.
        let incomplete = BrewPackagesModel(runner: await runner(try listing { $0["incomplete"] = ["bad"] }), log: { _ in })
        await incomplete.open()
        #expect(try #require(incomplete.listing).formulae.allSatisfy { incomplete.offer($0) == nil })
    }

    @Test("a listing that fails says why and keeps no old list")
    func failure() async throws {
        let runner = await runner(try Fixture.data("brew-packages"))
        let model = BrewPackagesModel(runner: runner, log: { _ in })
        await model.open()
        #expect(model.listing != nil)
        await runner.reply(.brewPackages, CLIRunner.Result(
            outcome: .error(status: 1), stdout: Data(),
            stderrTail: ["ddc: brew could not list its packages: /opt/homebrew/bin/brew is missing or did not answer"]
        ))
        await model.list()
        #expect(model.listing == nil)
        #expect(model.message == "brew could not list its packages: /opt/homebrew/bin/brew is missing or did not answer")
        #expect(await runner.arguments(of: .brewPackages) == Array(repeating: ["disk", "brew-packages", "--json"], count: 2))
        #expect(CLIRunner.Kind.brewPackages.hasLane, "the listing runs beside the queue")

        let missing = BrewPackagesModel(runner: nil, log: { _ in })
        await missing.open()
        #expect(missing.message?.contains("embedded CLI is missing") == true)
    }

    @Test("after a formula is uninstalled, what only it needed and nothing needs now is offered once")
    func orphansStep() async throws {
        let runner = await runner(try Fixture.data("brew-packages"))
        let model = BrewPackagesModel(runner: runner, log: { _ in })
        await model.open()
        let wget = try #require(model.listing.flatMap { $0.formulae.first { $0.fullName == "wget" } }.flatMap(model.offer))

        // A dry run that would run changes nothing and lists nothing again.
        await model.actionEnded(wget, try report(["action": "brew-uninstall", "result": ["status": "would-run"]]), dryRun: true)
        #expect(await runner.count(.brewPackages) == 1)
        #expect(model.orphans == nil)

        // Freed: brew is asked again, and only libidn2 is needed by nothing now.
        await runner.reply(.brewPackages, FakeRunner.done(try listing { json in
            var formulae = json["formulae"] as! [[String: Any]]
            formulae.remove(at: 1)
            json["formulae"] = formulae
            json["orphans"] = ["libidn2", "unrelated"]
        }))
        let freed = try report(["action": "brew-uninstall", "area": "homebrew", "identifier": "wget", "result": ["status": "freed"]])
        await model.actionEnded(wget, freed, dryRun: false)
        #expect(await runner.count(.brewPackages) == 2)
        let orphans = try #require(model.orphans)
        #expect(orphans.fullListed == #"{"action":"brew-uninstall-orphans","names":["libidn2"]}"#)
        #expect(orphans.bytes == 1_048_576 && orphans.buttonTitle == "Uninstall Unneeded…")
        #expect(orphans.consequence.hasPrefix("brew uninstall removes libidn2, which nothing installed needs now."))
        #expect(orphans.consequence.contains("stops at the first it cannot remove; the ones before it are already gone"))
        #expect(orphans.consequence.contains("Unlike brew autoremove"))
        #expect(orphans.consequence.contains("includes formulae built from source"))

        // The orphans' own run offers no further step.
        await model.actionEnded(orphans, try report(["action": "brew-uninstall-orphans", "result": ["status": "freed"]]), dryRun: false)
        #expect(model.orphans == nil)
        #expect(await runner.count(.brewPackages) == 3)

        // A refused dry run lists again, as brew changed; a failed run offers no orphans.
        await model.actionEnded(wget, try report(["action": "brew-uninstall", "result": ["status": "refused", "reason": "brew-kegs-changed"]]), dryRun: true)
        #expect(await runner.count(.brewPackages) == 4)
        await model.actionEnded(wget, nil, dryRun: false)
        #expect(model.orphans == nil)
        #expect(await runner.count(.brewPackages) == 5)

        // A closed sheet is not listed again.
        model.close()
        await model.actionEnded(wget, freed, dryRun: false)
        #expect(await runner.count(.brewPackages) == 5)
        // Nor is it by any other offer.
        await model.open()
        await model.actionEnded(AreaActions.Offer(action: .brewCleanup, identifier: nil, name: "Homebrew", bytes: 1), freed, dryRun: false)
        #expect(await runner.count(.brewPackages) == 6)
    }

    @Test("an orphans run that stopped offers again what is still needed by nothing")
    func orphansAgain() async throws {
        let runner = await runner(try listing { $0["orphans"] = ["libidn2", "libunistring"] })
        let model = BrewPackagesModel(runner: runner, log: { _ in })
        await model.open()
        let both = AreaActions.Offer(
            action: .brewUninstallOrphans, identifier: nil, name: "what wget needed", bytes: 6_291_456,
            names: ["libidn2", "libunistring"]
        )

        // Stopped before brew ran: both still needed by nothing, offered as before.
        await model.actionEnded(both, try report(["action": "brew-uninstall-orphans", "result": ["status": "not-run"]]), dryRun: false)
        #expect(await runner.count(.brewPackages) == 2)
        #expect(model.orphans == both)

        // brew removed libidn2 and stopped at libunistring: only it is offered again.
        await runner.reply(.brewPackages, FakeRunner.done(try listing { $0["orphans"] = ["libunistring", "unrelated"] }))
        let failed = try report(["action": "brew-uninstall-orphans", "result": ["status": "failed", "needs_admin": false]])
        await model.actionEnded(both, failed, dryRun: false)
        let rest = try #require(model.orphans)
        #expect(rest.fullListed == #"{"action":"brew-uninstall-orphans","names":["libunistring"]}"#)
        #expect(rest.name == "what wget needed" && rest.bytes == nil, "not listed as a formula: not measured")

        // A refused dry run and a run with no report offer the rest again too.
        let refused = try report(["action": "brew-uninstall-orphans", "result": ["status": "refused", "reason": "brew-needed"]])
        await model.actionEnded(rest, refused, dryRun: true)
        #expect(model.orphans == rest)
        await model.actionEnded(rest, nil, dryRun: false)
        #expect(model.orphans == rest)

        // Something came to need it: nothing is offered.
        await runner.reply(.brewPackages, FakeRunner.done(try listing { $0["orphans"] = ["unrelated"] }))
        await model.actionEnded(rest, failed, dryRun: false)
        #expect(model.orphans == nil)
        #expect(await runner.count(.brewPackages) == 6)
    }

    @Test("the report carries the kegs and what needs a refused formula; the sheet names them")
    func reportFields() throws {
        let refused = try report([
            "action": "brew-uninstall", "area": "homebrew", "identifier": "openssl@3",
            "result": ["status": "refused", "reason": "brew-needed"],
            "needed_by": [["formula": "openssl@3", "formulae": ["curl", "wget"], "casks": ["example-app"]]],
        ])
        #expect(refused.neededBy == [try decoded(#"{"formula":"openssl@3","formulae":["curl","wget"],"casks":["example-app"]}"#)])
        #expect(BrewWording.neededBy(refused) == "openssl@3 is needed by curl, wget and the cask example-app.")
        let preview = try report([
            "action": "brew-uninstall", "area": "homebrew", "identifier": "node", "result": ["status": "would-run"],
            "kegs": [
                ["formula": "node", "version": "22.1.0", "path": "/opt/homebrew/Cellar/node/22.1.0", "bytes": 104_857_600],
                ["formula": "node", "version": "23.0.0", "path": "/opt/homebrew/Cellar/node/23.0.0"],
            ],
        ])
        #expect(preview.kegs.map(\.version) == ["22.1.0", "23.0.0"] && preview.kegs[1].bytes == nil)
        #expect(preview.neededBy.isEmpty && BrewWording.neededBy(preview) == nil)
        let plain = try FreeReport.read(Fixture.data("free-report")).actions[0]
        #expect(plain.kegs.isEmpty && plain.neededBy.isEmpty, "absent on every other action")

        let ran = try report([
            "action": "brew-uninstall", "area": "homebrew", "identifier": "node", "result": ["status": "freed"],
            "output": "Uninstalling /opt/homebrew/Cellar/node/23.0.0... (2,000 files, 104MB)",
        ])
        #expect(FreeWording.showsOutput(ran) && !FreeWording.offersTerminal(ran), "the output's tail shows after the run")
        #expect(!FreeWording.showsOutput(plain))
    }

    private func decoded(_ json: String) throws -> FreeReport.NeededBy {
        try JSONDecoder.snakeCase.decode(FreeReport.NeededBy.self, from: Data(json.utf8))
    }
}

extension AreaActions.Offer {
    /// The list entry as the CLI reads it.
    var fullListed: String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.withoutEscapingSlashes, .sortedKeys]
        return String(decoding: try! encoder.encode(listed), as: UTF8.self)
    }
}
