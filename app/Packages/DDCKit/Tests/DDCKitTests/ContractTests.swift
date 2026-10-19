import Foundation
import Testing
@testable import DDCKit

/// The Swift side mirrors constants of the Rust core; these read the Rust
/// sources so a change there fails here instead of drifting silently.
@Suite("Contracts with the CLI")
struct ContractTests {
    @Test("the tombstone rule id is the core's")
    func tombstone() throws {
        let source = try rustSource("crates/ddc-core/src/rules/mod.rs")
        let match = try #require(source.firstMatch(of: /pub const TOMBSTONE: &str = "([^"]+)";/))
        #expect(String(match.1) == ReportSummary.tombstone)
    }

    @Test("the rules list version is the CLI's")
    func rulesVersion() throws {
        let source = try rustSource("crates/ddc-core/src/rules/listing.rs")
        let match = try #require(source.firstMatch(of: /pub const RULES_SCHEMA_VERSION: u32 = (\d+);/))
        #expect(Int(match.1) == RuleList.schemaVersion)
    }

    @Test("the settings version is the CLI's")
    func settingsVersion() throws {
        let source = try rustSource("crates/ddc-core/src/settings.rs")
        let match = try #require(source.firstMatch(of: /pub const SETTINGS_SCHEMA_VERSION: u32 = (\d+);/))
        #expect(Int(match.1) == SharedSettings.schemaVersion)
    }

    @Test("the default notification threshold is the background scan's")
    func defaultThreshold() throws {
        let source = try rustSource("crates/ddc-cli/src/cmd/background.rs")
        let match = try #require(source.firstMatch(of: /const DEFAULT_THRESHOLD: u64 = (\d+) << (\d+);/))
        #expect(try #require(UInt64(match.1)) << (try #require(UInt64(match.2))) == SharedSettings.defaultThreshold)
    }

    @Test("the startup disk is named as the CLI names it")
    func startupDisk() throws {
        let source = try rustSource("crates/ddc-cli/src/notify.rs")
        let match = try #require(source.firstMatch(of: /Some\("\/" \| "\/System\/Volumes\/Data"\) => "([^"]+)"/))
        #expect(Wording.volume("/") == String(match.1))
        #expect(Wording.volume("/System/Volumes/Data") == String(match.1))
        #expect(Wording.volume("/Volumes/Work") == "Work")
    }

    @Test("sizes are written as the CLI writes them")
    func sizes() {
        #expect(ByteSize.human(0) == "0 B")
        #expect(ByteSize.human(1023) == "1023 B")
        #expect(ByteSize.human(1536) == "1.5 KiB")
        #expect(ByteSize.human(10 << 30) == "10.0 GiB")
        #expect(ByteSize.human(1_048_575) == "1024.0 KiB")
    }

    @Test("the rules list and settings decode; a missing settings file gives the defaults")
    func decoding() throws {
        let rules = try Fixture.rules()
        #expect(rules.group(of: "node.node_modules") == "node_modules")
        #expect(rules.groups.first == "Python environments")
        let none = try SharedSettings.read(at: URL(filePath: "/nonexistent/settings.json"))
        #expect(none == SharedSettings())
        #expect(none.threshold == SharedSettings.defaultThreshold)
        let groups = try Fixture.groupsReport()
        #expect(groups.volumes.map(\.mountPoint) == ["/System/Volumes/Data"])
        #expect(groups.findings.contains { $0.notSelected == .activeProject(idleDays: 0) })
    }

    @Test("a settings file the CLI would refuse is refused, a newer one reads as newer")
    func strictSettings() throws {
        let data = try TempDataDirectory()
        func read(_ text: String) throws(SharedSettings.ReadError) -> SharedSettings {
            try? data.write(settings: text)
            return try SharedSettings.read(at: data.directory.settings)
        }
        let path = data.directory.settings.path
        #expect(throws: SharedSettings.ReadError.malformed(path: path, reason: "unknown key show_badge")) {
            try read(#"{"schema_version": 1, "show_badge": true}"#)
        }
        #expect(throws: SharedSettings.ReadError.otherSchema(2)) { try read(#"{"schema_version": 2, "min_size": "50M"}"#) }
        #expect(throws: SharedSettings.ReadError.malformed(path: path, reason: "Work is not an absolute path")) {
            try read(#"{"schema_version": 1, "roots": ["Work"]}"#)
        }
        #expect(throws: SharedSettings.ReadError.malformed(path: path, reason: "min_age_days is over 3650")) {
            try read(#"{"schema_version": 1, "min_age_days": 4000}"#)
        }
        #expect(throws: SharedSettings.ReadError.malformed(path: path, reason: "a field has the wrong type")) {
            try read(#"{"schema_version": 1, "notify_low_disk": "yes"}"#)
        }
        let full = try read(#"{"schema_version": 1, "roots": ["/Volumes/Work"], "excludes": ["/Volumes/Work/keep"], "min_age_days": 30, "notify_low_disk": false, "scheduler": {"by": "app", "app": "/Applications/DeepDriveCleaner.app"}, "min_size": 5}"#)
        #expect(full.roots == ["/Volumes/Work"] && full.minSize == 5)
    }

    @Test("the settings keys are the CLI's")
    func settingsKeys() throws {
        let source = try rustSource("crates/ddc-core/src/settings.rs")
        let body = try #require(source.firstMatch(of: /pub struct Settings \{([^}]*)\}/))
        let fields = Set(String(body.1).matches(of: /pub (\w+):/).map { String($0.1) })
        #expect(fields == SharedSettings.keys)
        let max = try #require(source.firstMatch(of: /const MAX_MIN_AGE_DAYS: u64 = (\d+);/))
        #expect(UInt64(max.1) == SharedSettings.maxMinAgeDays)
    }

    @Test("no source but CLIRunner.swift names the clean run kind")
    func cleanHasOneOwner() throws {
        let root = repositoryRoot()
        let dirs = ["app/DeepDriveCleaner", "app/Packages/DDCKit/Sources/DDCKit"]
        var offenders: [String] = []
        for dir in dirs {
            let files = try FileManager.default.contentsOfDirectory(atPath: root.appending(path: dir).path)
            for file in files where file.hasSuffix(".swift") && file != "CLIRunner.swift" {
                let text = try String(contentsOf: root.appending(path: "\(dir)/\(file)"), encoding: .utf8)
                if text.contains(/\.clean\b/) { offenders.append(file) }
            }
        }
        #expect(offenders.isEmpty, "the popover never cleans (ADR §7)")
    }
}
